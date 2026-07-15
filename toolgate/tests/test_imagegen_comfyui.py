"""Unit tests for the ComfyUI imagegen driver.

Covers graph templating (prompt/size/seed injection, node lookup by
class_type, overrides) and the full submit -> poll -> view generate() flow
with respx-mocked HTTP.
"""

import pytest
import respx
import httpx

from providers.imagegen_comfyui import ComfyUIImageGen, DEFAULT_WORKFLOW


BASE = "http://comfy-test:8188"


# ── graph templating ──────────────────────────────────────────────────────────

def test_build_graph_injects_prompt_size_seed():
    drv = ComfyUIImageGen(base_url=BASE)
    graph = drv._build_graph("a red apple", "768x512", None)

    assert graph["5"]["inputs"]["text"] == "a red apple"
    assert graph["8"]["inputs"]["width"] == 768
    assert graph["8"]["inputs"]["height"] == 512
    # seed is randomized (not the template's fixed 424242)
    assert graph["9"]["inputs"]["seed"] != DEFAULT_WORKFLOW["9"]["inputs"]["seed"]
    # template is not mutated (deep-copied)
    assert DEFAULT_WORKFLOW["5"]["inputs"]["text"] == ""


def test_build_graph_leaves_size_default_on_bad_input():
    drv = ComfyUIImageGen(base_url=BASE)
    graph = drv._build_graph("x", "not-a-size", None)
    assert graph["8"]["inputs"]["width"] == 1024
    assert graph["8"]["inputs"]["height"] == 1024


@pytest.mark.parametrize("size,expected", [
    ("1024x1024", (1024, 1024)),
    ("512X768", (512, 768)),
    ("100x0", None),
    ("abc", None),
    ("1024", None),
])
def test_parse_size(size, expected):
    assert ComfyUIImageGen._parse_size(size) == expected


def test_model_override_sets_unet_name():
    drv = ComfyUIImageGen(base_url=BASE)
    graph = drv._build_graph("x", "512x512", "other_checkpoint.safetensors")
    assert graph["1"]["inputs"]["unet_name"] == "other_checkpoint.safetensors"


def test_missing_prompt_node_raises():
    drv = ComfyUIImageGen(base_url=BASE, options={"workflow": {
        "1": {"class_type": "UNETLoader", "inputs": {}},
    }})
    with pytest.raises(ValueError, match="CLIPTextEncode"):
        drv._build_graph("x", "512x512", None)


def test_explicit_node_override_wins():
    wf = {
        "a": {"class_type": "CLIPTextEncode", "inputs": {"text": ""}},
        "b": {"class_type": "CLIPTextEncode", "inputs": {"text": ""}},
    }
    drv = ComfyUIImageGen(base_url=BASE, options={"workflow": wf, "nodes": {"prompt": "b"}})
    graph = drv._build_graph("hello", "512x512", None)
    assert graph["b"]["inputs"]["text"] == "hello"
    assert graph["a"]["inputs"]["text"] == ""


def test_seed_uses_noise_seed_field_when_present():
    wf = {
        "5": {"class_type": "CLIPTextEncode", "inputs": {"text": ""}},
        "9": {"class_type": "SamplerCustom", "inputs": {"noise_seed": 0}},
    }
    drv = ComfyUIImageGen(base_url=BASE, options={"workflow": wf})
    graph = drv._build_graph("x", "512x512", None)
    assert graph["9"]["inputs"]["noise_seed"] != 0
    assert "seed" not in graph["9"]["inputs"]


# ── output extraction ─────────────────────────────────────────────────────────

def test_first_image_finds_output():
    entry = {"outputs": {"11": {"images": [{"filename": "a.png", "subfolder": "", "type": "output"}]}}}
    assert ComfyUIImageGen._first_image(entry)["filename"] == "a.png"


def test_first_image_none_when_empty():
    assert ComfyUIImageGen._first_image({"outputs": {}}) is None
    assert ComfyUIImageGen._first_image({}) is None


def test_extract_error_reads_execution_error():
    status = {"messages": [
        ["execution_start", {}],
        ["execution_error", {"node_type": "KSampler", "exception_message": "OOM"}],
    ]}
    assert "KSampler" in ComfyUIImageGen._extract_error(status)
    assert "OOM" in ComfyUIImageGen._extract_error(status)


# ── full generate() flow ──────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_generate_happy_path(http_client):
    drv = ComfyUIImageGen(base_url=BASE, options={"comfy_poll_secs": 0})
    history = {"pid-1": {
        "status": {"status_str": "success", "completed": True},
        "outputs": {"11": {"images": [{"filename": "opex_0001.png", "subfolder": "", "type": "output"}]}},
    }}
    async with respx.mock() as mock:
        mock.post(f"{BASE}/prompt").mock(return_value=httpx.Response(200, json={"prompt_id": "pid-1"}))
        mock.get(f"{BASE}/history/pid-1").mock(return_value=httpx.Response(200, json=history))
        mock.route(method="GET", url__startswith=f"{BASE}/view").mock(
            return_value=httpx.Response(200, content=b"PNGBYTES"))
        out = await drv.generate(http_client, "a cat", "512x512")
    assert out == b"PNGBYTES"


@pytest.mark.asyncio
async def test_generate_polls_until_complete(http_client):
    drv = ComfyUIImageGen(base_url=BASE, options={"comfy_poll_secs": 0})
    pending = {"pid-2": {"status": {"status_str": "", "completed": False}, "outputs": {}}}
    done = {"pid-2": {
        "status": {"status_str": "success", "completed": True},
        "outputs": {"9": {"images": [{"filename": "x.png", "type": "output"}]}},
    }}
    async with respx.mock() as mock:
        mock.post(f"{BASE}/prompt").mock(return_value=httpx.Response(200, json={"prompt_id": "pid-2"}))
        mock.get(f"{BASE}/history/pid-2").mock(side_effect=[
            httpx.Response(200, json={}),          # entry not present yet
            httpx.Response(200, json=pending),     # queued, not done
            httpx.Response(200, json=done),        # finished
        ])
        mock.route(method="GET", url__startswith=f"{BASE}/view").mock(
            return_value=httpx.Response(200, content=b"IMG"))
        out = await drv.generate(http_client, "x", "512x512")
    assert out == b"IMG"


@pytest.mark.asyncio
async def test_generate_raises_on_execution_error(http_client):
    drv = ComfyUIImageGen(base_url=BASE, options={"comfy_poll_secs": 0})
    err = {"pid-3": {"status": {"status_str": "error", "messages": [
        ["execution_error", {"node_type": "KSampler", "exception_message": "boom"}],
    ]}}}
    async with respx.mock() as mock:
        mock.post(f"{BASE}/prompt").mock(return_value=httpx.Response(200, json={"prompt_id": "pid-3"}))
        mock.get(f"{BASE}/history/pid-3").mock(return_value=httpx.Response(200, json=err))
        with pytest.raises(ValueError, match="boom"):
            await drv.generate(http_client, "x", "512x512")


@pytest.mark.asyncio
async def test_generate_raises_when_no_prompt_id(http_client):
    drv = ComfyUIImageGen(base_url=BASE)
    async with respx.mock() as mock:
        mock.post(f"{BASE}/prompt").mock(return_value=httpx.Response(200, json={"error": "bad graph"}))
        with pytest.raises(ValueError, match="no prompt_id"):
            await drv.generate(http_client, "x", "512x512")
