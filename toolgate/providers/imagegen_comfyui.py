"""ComfyUI image generation provider (self-hosted, workflow-based).

Unlike the cloud imagegen drivers, ComfyUI has no OpenAI-style
`/images/generations` endpoint. A generation is a three-step dance:

  1. POST /prompt   {"prompt": <API-format graph>, "client_id": <uuid>}
                    -> {"prompt_id": ...}
  2. poll GET /history/{prompt_id} until the entry reports completion
  3. GET /view?filename=&subfolder=&type=  -> image bytes

The graph is model-specific (custom nodes, LoRAs, samplers), so rather than
hand-author one we ship the operator's working graph as the default template
and templatize only the three fields that vary per request: positive prompt
text, output size, and seed. Nodes are located by `class_type` (resilient to
node-id renumbering) with optional explicit id overrides via `options.nodes`.
The whole template + node mapping can be replaced from the Providers UI
(`options.workflow`, `options.nodes`) without a code change.

Content is passed through unfiltered — content support is a property of the
configured graph (LoRAs / checkpoint), not of this driver.

Relevant `options` keys (all optional, live in `providers.options`):
  workflow            full API-format graph dict (defaults to DEFAULT_WORKFLOW)
  nodes               {"prompt": "5", "size": "8", "seed": "9"} id overrides
  comfy_timeout_secs  poll deadline; cold model load can take ~2min (default 300)
  comfy_poll_secs     poll interval (default 1.5)
  timeouts.request_secs   per-HTTP-call timeout, shared with other drivers
"""

import asyncio
import copy
import random
import time
import urllib.parse
import uuid

import httpx

from providers.base import resolve_request_timeout


# Operator's working krea2-turbo graph, captured from ComfyUI /history.
# content-capable by design (the operator's installed HMcontent LoRA is node "4").
# Only nodes 5/8/9 (prompt / size / seed) are overwritten per request; every
# other node — checkpoint, CLIP, VAE, LoRA, conditioning rebalance — is used
# verbatim so behaviour matches the operator's ComfyUI exactly.
DEFAULT_WORKFLOW: dict = {
    "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "krea2_turbo_int8_convrot.safetensors", "weight_dtype": "default"}},
    "2": {"class_type": "CLIPLoader", "inputs": {"clip_name": "Huihui-Qwen3-VL-4B-Instruct-abliterated-fp8_scaled.safetensors", "type": "krea2", "device": "default"}},
    "3": {"class_type": "VAELoader", "inputs": {"vae_name": "krea2RealVae_v10.safetensors"}},
    "4": {"class_type": "LoraLoaderModelOnly", "inputs": {"lora_name": "krea2_lora.safetensors", "strength_model": 0.8, "model": ["1", 0]}},
    "5": {"class_type": "CLIPTextEncode", "inputs": {"text": "", "clip": ["2", 0]}},
    "6": {"class_type": "ConditioningKrea2Rebalance", "inputs": {"conditioning": ["5", 0], "multiplier": 4.0, "per_layer_weights": "1.0,1.0,1.0,1.0,1.0,1.0,1.0,2.5,5.0,1.1,4.0,1.0"}},
    "7": {"class_type": "ConditioningZeroOut", "inputs": {"conditioning": ["5", 0]}},
    "8": {"class_type": "EmptySD3LatentImage", "inputs": {"width": 1024, "height": 1024, "batch_size": 1}},
    "9": {"class_type": "KSampler", "inputs": {"model": ["4", 0], "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["8", 0], "seed": 424242, "steps": 8, "cfg": 1.0, "sampler_name": "euler", "scheduler": "simple", "denoise": 1.0}},
    "10": {"class_type": "VAEDecode", "inputs": {"samples": ["9", 0], "vae": ["3", 0]}},
    "11": {"class_type": "SaveImage", "inputs": {"images": ["10", 0], "filename_prefix": "opex"}},
}

# The one CLIPTextEncode we drive is the POSITIVE prompt. If a graph has more
# than one, set options.nodes.prompt to disambiguate.
_PROMPT_CLASSES = ("CLIPTextEncode",)
_SIZE_CLASSES = (
    "EmptySD3LatentImage",
    "EmptyLatentImage",
    "EmptyLatentImageAdvanced",
    "EmptySD3LatentImageAdvanced",
)
_SEED_CLASSES = ("KSampler", "KSamplerAdvanced", "SamplerCustom", "SamplerCustomAdvanced", "RandomNoise")

_MAX_SEED = 2**32 - 1


class ComfyUIImageGen:
    name = "ComfyUI"

    def __init__(self, base_url: str = "", api_key: str | None = None,
                 model: str | None = None, options: dict | None = None):
        self.base_url = (base_url or "http://127.0.0.1:8188").rstrip("/")
        # ComfyUI has no auth of its own; api_key is accepted but unused.
        self.model = (model or "").strip()  # optional UNET (checkpoint) override
        opts = options or {}
        self._request_timeout = resolve_request_timeout(opts, default=120.0)
        self._deadline = float(opts.get("comfy_timeout_secs", 300))
        self._poll_interval = float(opts.get("comfy_poll_secs", 1.5))
        wf = opts.get("workflow")
        self._workflow = wf if isinstance(wf, dict) and wf else DEFAULT_WORKFLOW
        nodes = opts.get("nodes")
        self._nodes = nodes if isinstance(nodes, dict) else {}

    # ── graph templating ──────────────────────────────────────────────────────

    def _find_node(self, graph: dict, kind: str, classes: tuple) -> str | None:
        """Locate a node id by explicit override (options.nodes.<kind>) first,
        then by matching class_type. Returns None if no candidate exists."""
        nid = self._nodes.get(kind)
        if isinstance(nid, str) and nid in graph:
            return nid
        for node_id, node in graph.items():
            if isinstance(node, dict) and node.get("class_type") in classes:
                return node_id
        return None

    def _build_graph(self, prompt: str, size: str, model: str | None) -> dict:
        graph = copy.deepcopy(self._workflow)

        prompt_node = self._find_node(graph, "prompt", _PROMPT_CLASSES)
        if prompt_node is None:
            raise ValueError("ComfyUI workflow has no CLIPTextEncode node to carry the prompt")
        graph[prompt_node].setdefault("inputs", {})["text"] = prompt

        size_node = self._find_node(graph, "size", _SIZE_CLASSES)
        if size_node is not None:
            dims = self._parse_size(size)
            if dims is not None:
                w, h = dims
                graph[size_node].setdefault("inputs", {})["width"] = w
                graph[size_node]["inputs"]["height"] = h

        # Randomize the seed each call so repeat prompts don't return the same
        # cached latent. (random is fine here — this is toolgate, not a
        # replayable workflow script.)
        seed_node = self._find_node(graph, "seed", _SEED_CLASSES)
        if seed_node is not None:
            seed = random.randint(0, _MAX_SEED)
            inputs = graph[seed_node].setdefault("inputs", {})
            if "noise_seed" in inputs:
                inputs["noise_seed"] = seed
            else:
                inputs["seed"] = seed

        mdl = (model or self.model or "").strip()
        if mdl:
            for node in graph.values():
                if isinstance(node, dict) and node.get("class_type") == "UNETLoader":
                    node.setdefault("inputs", {})["unet_name"] = mdl
                    break

        return graph

    @staticmethod
    def _parse_size(size: str) -> tuple[int, int] | None:
        try:
            w_str, h_str = str(size).lower().split("x", 1)
            w, h = int(w_str), int(h_str)
            if w > 0 and h > 0:
                return w, h
        except (ValueError, AttributeError):
            pass
        return None

    # ── generation ────────────────────────────────────────────────────────────

    async def generate(self, http: httpx.AsyncClient, prompt: str,
                       size: str = "1024x1024", model: str | None = None,
                       quality: str = "standard") -> bytes:
        graph = self._build_graph(prompt, size, model)
        client_id = uuid.uuid4().hex

        submit = await http.post(
            f"{self.base_url}/prompt",
            json={"prompt": graph, "client_id": client_id},
            timeout=self._request_timeout,
        )
        submit.raise_for_status()
        prompt_id = submit.json().get("prompt_id")
        if not prompt_id:
            raise ValueError(f"ComfyUI /prompt returned no prompt_id: {submit.text[:200]}")

        entry = await self._await_completion(http, prompt_id)

        image = self._first_image(entry)
        if image is None:
            raise ValueError("ComfyUI run completed but produced no image output")

        query = urllib.parse.urlencode({
            "filename": image.get("filename", ""),
            "subfolder": image.get("subfolder", ""),
            "type": image.get("type", "output"),
        })
        view = await http.get(f"{self.base_url}/view?{query}", timeout=self._request_timeout)
        view.raise_for_status()
        if not view.content:
            raise ValueError("ComfyUI /view returned empty body")
        return view.content

    async def _await_completion(self, http: httpx.AsyncClient, prompt_id: str) -> dict:
        deadline = time.monotonic() + self._deadline
        while time.monotonic() < deadline:
            resp = await http.get(
                f"{self.base_url}/history/{prompt_id}",
                timeout=self._request_timeout,
            )
            resp.raise_for_status()
            entry = resp.json().get(prompt_id)
            if entry:
                status = entry.get("status", {}) or {}
                if status.get("status_str") == "error":
                    raise ValueError(f"ComfyUI execution failed: {self._extract_error(status)}")
                if status.get("completed") or status.get("status_str") == "success":
                    return entry
            await asyncio.sleep(self._poll_interval)
        raise TimeoutError(
            f"ComfyUI did not finish prompt {prompt_id} within {self._deadline:.0f}s"
        )

    @staticmethod
    def _first_image(entry: dict) -> dict | None:
        outputs = entry.get("outputs", {}) or {}
        for node_output in outputs.values():
            images = node_output.get("images") if isinstance(node_output, dict) else None
            if images:
                return images[0]
        return None

    @staticmethod
    def _extract_error(status: dict) -> str:
        for message in status.get("messages", []) or []:
            if isinstance(message, (list, tuple)) and len(message) == 2 and message[0] == "execution_error":
                data = message[1] or {}
                node = data.get("node_type", "?")
                exc = data.get("exception_message", "unknown error")
                return f"{node}: {exc}"
        return "unknown error"
