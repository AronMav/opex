---
name: image-prompt-advisor
description: Expert image prompt advisor that searches a curated library of 12,000+ professional prompts and adapts them for any image generation model (DALL-E, FLUX, Stable Diffusion, etc.). Use this skill when the user wants high-quality image generation results, asks to "find a good prompt", "suggest a style", "generate something like X", or wants help crafting prompts for impressive results. Complements image-generation.md — use this one when prompt quality matters, not just speed.
triggers:
  - найди промпт
  - подбери стиль
  - подбери промпт
  - find a prompt
  - suggest a style
  - image style
  - prompt advisor
  - prompt library
  - улучши промпт
  - improve prompt
  - professional prompt
  - best prompt
  - prompt for image
  - style like
  - в стиле
priority: 8
---

# Image Prompt Advisor

Searches a curated library of 12,000+ professional prompts across 11 categories, adapts them to the user's subject, and generates via `generate_image` for any configured model.

## Prompt Library

Hosted on GitHub CDN — fetched on demand, never loaded in full.

**Manifest** (categories index):
```
https://raw.githubusercontent.com/YouMind-OpenLab/nano-banana-pro-prompts-recommend-skill/main/references/manifest.json
```

**Per-category file**:
```
https://raw.githubusercontent.com/YouMind-OpenLab/nano-banana-pro-prompts-recommend-skill/main/references/{slug}.json
```

Each prompt entry has: `name`, `prompt`, `negative_prompt`, `tags`, `preview_url` (optional).

## Workflow

### Step 0 — Load Manifest

Check if `workspace/refs/image-prompts-manifest.json` exists and was modified within the last 7 days using `workspace_read`. If missing or stale, fetch it:

```
process(action="start", command="mkdir -p ~/hydeclaw/workspace/refs && curl -sL 'https://raw.githubusercontent.com/YouMind-OpenLab/nano-banana-pro-prompts-recommend-skill/main/references/manifest.json' -o ~/hydeclaw/workspace/refs/image-prompts-manifest.json")
```

Then read the manifest to see available categories and their slugs.

### Step 1 — Understand the Request

If the user's intent is clear enough to pick a category ("sci-fi city", "portrait in oil painting style"), skip clarification and go straight to Step 2.

If intent is ambiguous, ask **one** question — the most important missing piece:
- Subject / theme?
- Mood / atmosphere?
- Preferred style (photorealistic, illustration, painting, anime)?

Do not interrogate. One question max, then proceed.

### Step 2 — Search Relevant Prompts

Pick the most fitting category from the manifest. Fetch **only that category's** JSON and filter by keywords — never load full files into context:

```
process(action="start", command="curl -sL 'https://raw.githubusercontent.com/YouMind-OpenLab/nano-banana-pro-prompts-recommend-skill/main/references/{slug}.json' | python3 -c \"import json,sys; data=json.load(sys.stdin); matches=[p for p in data if any(k in json.dumps(p).lower() for k in ['{kw1}','{kw2}'])]; [print(json.dumps(m)) for m in matches[:8]]\"")
```

If a category is borderline, check at most 2 — don't fetch all 11.

Extract 3–5 best-matching prompts from the results.

### Step 3 — Present Options

Show 3–5 prompts as named style options. For each:
- A short style label
- The prompt text (truncate to ~80 words if long)

```
**Option 1 — Cinematic Neon Noir**
A lone figure walks through rain-slicked streets, neon reflections on wet asphalt, dramatic side lighting, photorealistic, ultra-detailed, shallow depth of field, no text

**Option 2 — Ethereal Dreamscape**
Floating islands suspended in golden mist, waterfalls cascading into clouds, fantasy illustration, warm light, painterly style, no text
```

Offer to preview any option at draft quality before committing:
> "Want me to generate a quick draft of any of these? Or pick one and I'll adapt it to your subject."

### Step 4 — No-Match Fallback

If the library has nothing relevant, don't apologize — build a custom prompt using the template:

```
[Subject] [Action/Pose], [Setting/Background], [Style], [Lighting], [Color palette], [Camera/Composition], no text, no watermark
```

Then proceed to Step 5 with the custom prompt.

### Step 5 — Adapt and Generate

1. **Adapt to subject** — replace the library prompt's generic subject/elements with the user's specific subject. Keep the style skeleton.

2. **Select size**:
   | Content | Size |
   |---|---|
   | Landscape, wide scene, banner | `1792x1024` |
   | Portrait, poster, vertical | `1024x1792` |
   | Character, icon, square art | `1024x1024` |
   | Quick concept check | `512x512` |

3. **Select quality** — `standard` for iterations, `high` for finals.

4. **Select model** — if the user specified one, pass it. Otherwise omit (uses configured default).

5. **Generate**:
```
generate_image(
  prompt="[adapted English prompt]",
  size="1024x1024",
  quality="standard"
)
```

### Step 6 — Iterate

After showing the result:
- Point out 1–2 specific things that could be refined (lighting? color? composition? detail level?)
- Ask what the user wants to change
- Regenerate immediately on feedback — no permission needed, just confirm what you're changing

## Model Parameter

The `model` parameter accepts provider-specific identifiers. Omit it to use the system default.

Common values (depend on what's configured on this HydeClaw instance):
- `dall-e-3` — OpenAI DALL-E 3
- `flux-pro`, `flux-dev`, `flux-schnell` — FLUX family
- `sd3-large`, `sd3-medium` — Stable Diffusion 3
- `imagen-3` — Google Imagen 3

When the user names a model or asks "use FLUX / use DALL-E", pass `model=` accordingly.

## Prompt Rules

- **Always write prompts in English** — translate the user's request yourself
- **Specific beats abstract** — "a weathered leather satchel, warm afternoon light, macro lens" beats "a bag"
- **Always anchor the style** — without an explicit style, results are unpredictable
- **50–100 words optimal** — longer prompts get partially ignored by most providers
- **End with negatives** — `no text, no watermark, no blur, no extra limbs`
