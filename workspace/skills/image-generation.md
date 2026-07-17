---
name: image-generation
description: >
  High-quality image generation via generate_image — includes prompt crafting rules,
  a curated prompt library (12,000+ professional prompts), and an adaptive workflow.
  Use whenever the user asks to draw, create, generate, visualize, show an image,
  find/suggest a prompt, improve a prompt, or emulate a style.
triggers:
  - нарисуй
  - сгенерируй
  - нарисовать
  - создай изображение
  - создай картинку
  - сделай картинку
  - покажи как выглядит
  - иллюстрация
  - арт
  - draw
  - generate image
  - create image
  - image of
  - picture of
  - visualize
  - sketch
  - photo of
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
priority: 10
state: active
---

# Image Generation

## Tool

```
generate_image(prompt, negative_prompt?, size?, quality?, model?)
```

| Parameter | Values | Default |
|-----------|--------|---------|
| `prompt` | English description of **what you want** — never "no X" phrases | required |
| `negative_prompt` | Optional — **what to avoid** (`blurry, extra fingers, deformed hands, watermark`). Honored by the local Chroma model; empty keeps its built-in quality negative. | — |
| `size` | Any `WxH` up to **2048×2048** (2K). Each side 512–2048, multiples of 64. **You choose** the best fit for the content. | `1024x1024` |
| `quality` | **No effect** — the model pipeline is fixed. Leave default. | `standard` |
| `model` | Leave empty — single local model (see below). | auto |

### Model Parameter

This instance generates through a **single local model** — ComfyUI running a Flux-family checkpoint ("krea2"). There is no cloud model menu, so **leave `model` empty**. (If set, it overrides the ComfyUI checkpoint name — you almost never need this.) There is likewise no separate `quality` mode: every image runs the same fast pipeline, and detail is controlled by `size` and the prompt, not by `quality`.

---

## Size Selection

You are free to pick **any** `WxH` (multiples of 64) up to **2048** per side — choose what fits the content. Larger = more detail but slower. Guide:

| Content type | Typical size | For max detail |
|---|---|---|
| Logo, icon, avatar, square art | `1024x1024` | `2048x2048` |
| Landscape, interior, wide scene, wallpaper | `1536x1024`, `1344x768` | `2048x1152` |
| Portrait, poster, book cover, vertical | `1024x1536`, `768x1344` | `1152x2048` |
| Quick draft / concept check | `512x512` | — |

---

## Prompt Library

A curated library of 12,000+ professional prompts across 11 categories, hosted on GitHub CDN.

**Manifest** (categories index):
```
https://raw.githubusercontent.com/YouMind-OpenLab/nano-banana-pro-prompts-recommend-skill/main/references/manifest.json
```

**Per-category file**:
```
https://raw.githubusercontent.com/YouMind-OpenLab/nano-banana-pro-prompts-recommend-skill/main/references/{slug}.json
```

Each entry: `name`, `prompt`, `negative_prompt`, `tags`, `preview_url` (optional).

### Loading the Manifest

Check if `workspace/refs/image-prompts-manifest.json` exists and was modified within the last 7 days using `workspace_read`. If missing or stale, fetch:

```
code_exec(language="bash", code="mkdir -p ~/opex/workspace/refs && curl -sL 'https://raw.githubusercontent.com/YouMind-OpenLab/nano-banana-pro-prompts-recommend-skill/main/references/manifest.json' -o ~/opex/workspace/refs/image-prompts-manifest.json")
```

### Searching Prompts

Pick the most fitting category from the manifest. Fetch **only that category's** JSON and filter by keywords — never load full files into context:

```
code_exec(language="bash", code="curl -sL 'https://raw.githubusercontent.com/YouMind-OpenLab/nano-banana-pro-prompts-recommend-skill/main/references/{slug}.json' | python3 -c \"import json,sys; data=json.load(sys.stdin); matches=[p for p in data if any(k in json.dumps(p).lower() for k in ['{kw1}','{kw2}'])]; [print(json.dumps(m)) for m in matches[:8]]\"")
```

If a category is borderline, check at most 2 — don't fetch all 11. Extract 3–5 best-matching prompts.

---

## Workflow

### Step 1 — Understand the Request

If intent is clear enough to pick a category or style, skip clarification and proceed. If ambiguous, ask **one** question max:
- Subject / theme?
- Mood / atmosphere?
- Preferred style?

### Step 2 — Source the Prompt

**Option A — Library match** (when the request maps to a known style/category):
1. Load manifest (if stale)
2. Fetch relevant category JSON
3. Extract 3–5 best-matching prompts

**Option B — Custom prompt** (when the library has nothing relevant):
Build using the template below — no apology needed.

### Step 3 — Present Options (if multiple)

Show 3–5 prompts as named style options. For each:
- A short style label
- The prompt text (truncate to ~80 words if long)

```
**Option 1 — Cinematic Neon Noir**
A lone figure walks through rain-slicked streets, neon reflections on wet asphalt, dramatic side lighting, photorealistic, ultra-detailed, shallow depth of field, sharp focus
```

Offer to preview a quick draft (small size, e.g. 512x512) before committing.

### Step 4 — Draft → Final

For non-trivial requests, validate cheaply first:

1. **Draft** — `size=512x512`. Fast. Validates direction.
2. **Final** — `size=1024x1024` or larger (up to 2048 per side). Once direction is confirmed.

Tell the user you're generating a draft and offer to refine after they see it.

### Step 5 — Generate

1. **Adapt** — if using a library prompt, replace generic subject/elements with the user's specifics. Keep the style skeleton.
2. **Select size** (see table above) — pick the `WxH` that fits the content, up to 2K.
3. **Generate**:
```
generate_image(prompt="[adapted English prompt]", negative_prompt="blurry, deformed hands, extra fingers, watermark", size="1024x1024")
```

### Step 6 — Iterate

After showing the result:
- Point out 1–2 specific things that could be refined (lighting? color? composition? detail level?)
- Ask what the user wants to change
- Regenerate immediately on feedback — no permission needed, just confirm what you're changing

---

## Prompt Crafting Rules

- **Always write in English** — the model (a local Flux-family model via ComfyUI) understands English best. Translate the user's request yourself.
- **Specific > abstract** — "a red sports car parked in front of a modern glass building at dusk" beats "beautiful car".
- **Always specify style** — without an explicit style the result is unpredictable. If the user didn't specify, pick one (photorealistic for realistic scenes, digital art for game/fantasy content).
- **50–100 words optimal** — longer prompts get partially ignored.
- **Split positive vs negative** — put ONLY what you want in `prompt`; put what to AVOID in the separate `negative_prompt` param (`blurry, extra fingers, deformed hands, watermark`). Never cram `no X` into `prompt` — naming a thing in the positive prompt tends to *draw* it. The active local model (Chroma) honors `negative_prompt`; even if a model doesn't, its built-in quality negative still applies, so positive-only prompts stay safe.

### Prompt Structure

```
[Subject] [Action/Pose], [Setting/Background], [Style], [Lighting], [Color palette], [Camera/Composition]
```

**Elements to include as needed:**

- **Subject** — who/what is primary: `a young woman reading`, `a futuristic city`, `a golden retriever`
- **Style** — `photorealistic`, `oil painting`, `watercolor`, `anime`, `digital art`, `3D render`, `minimalist illustration`, `cinematic`, `sketch`
- **Lighting** — `golden hour light`, `soft studio lighting`, `dramatic side lighting`, `neon glow`, `overcast daylight`
- **Composition** — `close-up portrait`, `wide establishing shot`, `bird's eye view`, `rule of thirds`, `centered symmetry`
- **Color palette** — `warm earth tones`, `cool blues and purples`, `monochrome`, `vibrant saturated colors`
- **Cleanliness (positive form)** — `clean background`, `sharp focus`, `flawless detail` — not `no clutter` / `no blur`

**Good prompt example:**
```
A lone astronaut standing on the surface of Mars, looking at Earth rising above the horizon,
photorealistic, dramatic cinematic lighting, orange-red dust and rocks in foreground,
deep blue Earth in distance, ultra-detailed spacesuit, wide establishing shot,
volumetric atmosphere, sharp focus
```

---

## Common Scenarios

**Avatar / character portrait:**
```
Portrait of [description], facing camera, [style], professional headshot composition,
clean background, high detail on face, soft lighting
```

**Logo / icon:**
```
Minimalist flat logo design for [concept], simple geometric shapes,
[color1] and [color2] palette, white background, vector style, clean lines
```

**Story illustration:**
```
Illustration for a story about [topic], [mood] atmosphere, [style],
[key visual elements from the text], clean composition
```

**Product / UI concept art:**
```
Product concept render of [description], clean studio shot,
3D render, professional product photography lighting, white background
```

**Wallpaper / background:**
```
[Scene description], desktop wallpaper, ultra wide 1792x1024,
[style], [mood], no subjects in center (leave space for icons)
```
