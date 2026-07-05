# Model metadata multi-catalog (context windows + provider set)

Status: **proposed** · Author: session 2026-07-05 · Supersedes the ad-hoc
`default_context_for_model` heuristic as the primary window source.

## Problem

Model context windows are today determined by, in order: manual per-model
override → provider API probe (`/api/show`, `/v1/models`) → name heuristic
(`default_context_for_model`). The heuristic is wrong for most modern models and
the probe fails for providers whose API doesn't report the window (MiMo → 128k).
We also have no complete provider set — `PROVIDER_TYPES` is 30 hand-maintained
entries vs. the hundreds real aggregators track — and no cost/capability data.

## Goal

A **multi-catalog** metadata layer, modelled on opencode's models.dev integration
but stronger (opencode is catalog-or-nothing; we keep native probing for the long
tail). Phase 1 delivers authoritative automatic context windows for all catalogued
models; later phases add the provider picker and cost/capability data.

## Sources (researched 2026-07-05)

| Source | Fields | Coverage | Access |
| --- | --- | --- | --- |
| **models.dev** `/api.json` | `limit.context/output`, cost, caps, modalities | broad; western + alibaba/zhipu/moonshot | public |
| **OpenRouter** `/api/v1/models` | `context_length`, `top_provider.max_completion_tokens`, pricing, modalities | 100+; **best Chinese** (deepseek/qwen/glm/kimi/minimax/tencent/xiaomi) | public, no auth |
| **LiteLLM** `model_prices_and_context_window.json` | `max_input_tokens/max_output_tokens`, cost, caps | 500+; strong Bedrock/Azure/western, weak Chinese | public (GitHub raw) |
| **Native provider APIs** | see below | authoritative for the exact deployment | per-provider |

Native self-report endpoints (most authoritative — know the deployment's real
`num_ctx`/`max_model_len`, not just the model's max):

- ollama (incl. ollama.com) `/api/show` → `*.context_length` *(implemented)*
- vLLM / SGLang `/v1/models` → `max_model_len` / `max_seq_len` *(implemented)*
- Google Gemini `models.list` → `inputTokenLimit`/`outputTokenLimit` *(new)*
- Groq `/openai/v1/models` → `context_window`; Mistral → `max_context_length`;
  Together → `context_length` *(openai-compat probe already reads these keys)*
- OpenAI / Anthropic `/v1/models` → no window → catalog only

## Resolution priority (extends the existing chain)

```
manual per-model override                                   (implemented)
  > native provider self-report (ollama/vllm/google/groq/mistral/together)
  > multi-catalog  (models.dev ∪ OpenRouter ∪ LiteLLM?)      (NEW — this spec)
  > name heuristic                                          (implemented)
```

Native beats catalog: a local ollama with a custom `num_ctx` is more correct than
the catalog's model-max. Catalog fills where the probe returns `None`
(openai/anthropic/deepseek/moonshot/qwen/glm/minimax/xai/xiaomi/…).

## Per-provider-type source map

- **openai, anthropic** → catalog (models.dev / LiteLLM)
- **google, gemini-cli, gemini-cloudcode** → native `models.list`, then catalog
- **groq, mistral, together, ollama, vllm, sglang** → native self-report
- **deepseek, moonshot, qwen, glm, minimax, xai, perplexity, nvidia, xiaomi(MiMo)** → OpenRouter catalog ∪ models.dev
- **openrouter, litellm, huggingface** → own listing (`/api/v1/models`, HF Hub `config.json` `max_position_embeddings`)
- **volcengine(doubao), qianfan(ernie)** → weak in aggregators → manual override (Doubao Seed 256K, MiniMax 204,800 from docs)
- **claude-cli, codex-cli** → delegate to vendor catalog (anthropic/openai)

## Module design

`crates/opex-core/src/agent/providers/catalog/`:

- `mod.rs` — `ModelCatalog` held in `AppState`; in-memory index
  `HashMap<CatalogKey, ModelMeta>` where `ModelMeta { context: u32, output:
  Option<u32>, cost: Option<Cost>, caps: Caps, modalities: Modalities, source:
  Source }`. Secondary loose index by normalized model-id for provider-agnostic
  fallback match.
- `models_dev.rs`, `openrouter.rs`, `litellm.rs` (optional) — one loader each:
  fetch → parse → normalize → emit entries tagged with source priority
  (models.dev > OpenRouter > LiteLLM on conflict).
- Fetch/cache (mirrors opencode): disk cache `~/opex/cache/catalog/{source}.json`,
  TTL default 24h (server, not per-CLI), **bundled snapshot** shipped in the
  release (`scaffold/catalog/{source}.json`) for offline/first-run, background
  refresh task, atomic temp→rename write, all errors non-fatal (log + ignore).
- Config `config/opex.toml`:
  ```toml
  [model_catalog]
  enabled = true
  refresh_hours = 24
  sources = ["models_dev", "openrouter"]   # litellm optional
  models_dev_url = "https://models.dev/api.json"
  openrouter_url = "https://openrouter.ai/api/v1/models"
  ```

### Source structure (loaders differ)

- **models.dev** is an object **keyed by provider id**, each with a nested
  `models{}` map → loader iterates providers then models, `provider_id` is the key.
- **OpenRouter** is a **flat `data[]`** of models whose `id` is `vendor/model`
  (e.g. `moonshotai/kimi-k2`) → loader derives `provider_id` from the slug prefix
  and `model_id` from the suffix. No provider-level object.

Both normalise into the same `HashMap<CatalogKey, ModelMeta>`.

### ID normalization

- `provider_type` → catalog provider id(s): e.g. `glm→[zhipu, z-ai]`,
  `qwen→[alibaba, dashscope]`, `moonshot→[moonshotai]`, `xiaomi→[xiaomi]`,
  `minimax→[minimax]`, `deepseek→[deepseek]`. Table in `catalog/aliases.rs`.
- model id: strip `:cloud`/tags, lowercase; try exact `(provider_id, model)`,
  then alias table (`kimi-k2.6 → moonshotai/kimi-k2`), then loose model-id match.
- `limit.context` / `context_length` = the model's **total window** (matches how
  OPEX already uses `compressor.context_limit`), not input-only.
- **Coverage is best-effort.** A model absent from every catalog falls through to
  the native probe / heuristic / manual override — the catalog never regresses a
  currently-working resolution. Specifically `mimo-v2.5-pro` (served from
  xiaomimimo, not OpenRouter's own routing) may NOT be in any catalog and can
  remain a manual-override case; the multi-catalog is an additive improvement, not
  a guaranteed fix for every exotic model.

### Startup & IO

- On startup, load the **bundled snapshot synchronously** into the index so the
  first session bootstrap resolves against a warm catalog (no cold-miss →
  heuristic-cache poisoning). Disk-cache and remote refresh run in the background.
- Catalog fetch uses the **standard `http_client()`** — the source URLs are
  admin-configured in `opex.toml` (trusted, like the internal-endpoint allowlist),
  not agent-supplied; no per-request SSRF resolver needed. (If we ever accept a
  user-supplied catalog URL, switch that path to `ssrf_http_client()`.)

### Resolution integration

- Extend `resolve_context_limit`: after the native `context_limit_hint` returns
  `None`, query `ModelCatalog::context(provider_type, model)` before the
  heuristic. Cache the result (non-fallback) so `context_limit_tokens` (sync
  hot-path) picks it up unchanged.

### UI surface

- Extend `ModelInfo` (returned by `GET /api/providers/{id}/models`) with
  `context_window: Option<u32>`, resolved via native-probe/catalog. The provider
  dialog's per-model table then shows the resolved number in the `auto`
  placeholder (`auto · 128000`) instead of a bare `auto`.

### Testing (TDD)

- **Parse + normalize:** fixture JSONs (`models.dev` object shape, OpenRouter flat
  shape) → assert the index has the expected `(provider_id, model_id) → context`.
- **Alias / lookup:** `kimi-k2.6` resolves to `moonshotai/kimi-k2`; unknown model
  → `None`; loose model-id match when provider unknown.
- **Resolution priority:** native hint present → wins; native `None` + catalog hit
  → catalog; both `None` → heuristic. (Pure functions + the existing
  `context_window_tests` module.)
- Network fetch/cache is not unit-tested (IO); covered by the snapshot-load path +
  a manual `POST /api/catalog/refresh` smoke on the server.

## Phasing

- **Phase 1a (start here):** `ModelCatalog` service + `models_dev` loader
  (snapshot + cache + background refresh) + `catalog/aliases` + resolution
  integration + fixture tests. End-to-end pipeline through one source.
- **Phase 1b:** add the `openrouter` loader (second source behind the same trait)
  + merge/priority; broadens Chinese coverage.
- **Phase 1c:** Google native `models.list` hint; extend `ModelInfo` with
  `context_window` so the UI table shows `auto · N`.
- **Phase 2:** provider preset picker in the "add provider" flow, sourced from the
  catalog (hundreds of providers with pre-filled base_url/env/models).
- **Phase 3:** use `cost` (usage $ tracking), `caps` (gate vision/tools per
  model), `limit.output` (response cap).

Broadly this auto-resolves the great majority of catalogued models; the few not in
any catalog (e.g. `mimo-v2.5-pro`) keep working via native probe / manual override
(already shipped) — no regression.

## Open questions

1. Include LiteLLM in Phase 1 (adds Bedrock/Azure) or defer? Default: defer.
2. Refresh cadence — 24h vs 6h. Default: 24h + on-demand `POST
   /api/catalog/refresh`.
3. Snapshot update process — a `scripts/pull-catalog.ts` (or make target) that
   fetches latest into `scaffold/catalog/` for the next release.
4. Do we normalise per-1M cost units across sources (models.dev per-1M vs
   LiteLLM per-token)? Only relevant for Phase 3.
