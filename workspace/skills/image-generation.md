---
name: image-generation
description: How to generate high-quality images using the generate_image tool. Use this skill whenever the user asks to draw, create, generate, visualize, or show an image — including requests like "draw", "make a picture", "show me what it looks like", "create an illustration", "generate a photo", or any description of a desired visual. Apply even when the user just describes a scene or idea without explicitly using the word "image".
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
priority: 10
---

## Tool

```
generate_image(prompt, size?, quality?, model?)
```

| Parameter | Values | Default |
|-----------|--------|---------|
| `prompt` | Text description — always in English | required |
| `size` | `1024x1024` · `1792x1024` · `1024x1792` · `512x512` | `1024x1024` |
| `quality` | `standard` (fast) · `high` (28 steps, slower, better) | `standard` |
| `model` | optional: specific provider model override | auto |

---

## Size Selection

| Content type | Size |
|---|---|
| Landscape, interior, wide scene, banner, desktop wallpaper | `1792x1024` |
| Portrait, poster, book cover, vertical banner | `1024x1792` |
| Logo, icon, avatar, square art | `1024x1024` |
| Quick draft / concept check | `512x512` |

---

## Draft → Final Workflow

For non-trivial requests, check the concept cheaply first:

1. **Draft** — `size=512x512`, `quality=standard`. Fast. Validates direction.
2. **Final** — `size=1024x1024` or larger, `quality=high`. Once direction is confirmed.

Tell the user you're generating a draft and offer to refine after they see it.

---

## Prompt Structure

Output quality depends directly on the prompt. Build it like this:

```
[Subject] [Action/Pose], [Setting/Background], [Style], [Lighting], [Color palette], [Camera/Composition], [What to avoid]
```

**Elements to include as needed:**

- **Subject** — who/what is primary: `a young woman reading`, `a futuristic city`, `a golden retriever`
- **Style** — `photorealistic`, `oil painting`, `watercolor`, `anime`, `digital art`, `3D render`, `minimalist illustration`, `cinematic`, `sketch`
- **Lighting** — `golden hour light`, `soft studio lighting`, `dramatic side lighting`, `neon glow`, `overcast daylight`
- **Composition** — `close-up portrait`, `wide establishing shot`, `bird's eye view`, `rule of thirds`, `centered symmetry`
- **Color palette** — `warm earth tones`, `cool blues and purples`, `monochrome`, `vibrant saturated colors`
- **Negative hints** — `no text`, `no watermark`, `no blur`, `no extra limbs`

**Good prompt example:**
```
A lone astronaut standing on the surface of Mars, looking at Earth rising above the horizon,
photorealistic, dramatic cinematic lighting, orange-red dust and rocks in foreground,
deep blue Earth in distance, ultra-detailed spacesuit, wide establishing shot,
volumetric atmosphere, no text, no watermark
```

---

## Prompt Rules

- **Always write in English** — all providers (FLUX, Stable Diffusion, DALL-E) understand English best. Translate the user's request yourself.
- **Specific > abstract** — "a red sports car parked in front of a modern glass building at dusk" beats "beautiful car".
- **Always specify style** — without an explicit style the result is unpredictable. If the user didn't specify, pick one (photorealistic for realistic scenes, digital art for game/fantasy content).
- **Don't overload** — 50–100 words is optimal. Longer and the provider may ignore parts of the prompt.

---

## Iteration

After showing the image to the user:
- If the result isn't right — ask what to change (style? colors? composition? details?)
- Regenerate with a refined prompt without asking permission again
- For style transfer / edits — describe the changes explicitly in the prompt

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
[key visual elements from the text], no text overlay
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
