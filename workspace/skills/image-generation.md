---
name: image-generation
description: Как генерировать высококачественные изображения с помощью инструмента generate_image. Используй этот скилл всегда, когда пользователь просит нарисовать, создать, сгенерировать, визуализировать или показать изображение — включая запросы вроде «нарисуй», «сделай картинку», «покажи как выглядит», «создай иллюстрацию», «сгенерируй фото» или любые описания желаемого визуала. Применяй даже если пользователь просто описывает сцену или идею, явно не используя слово «изображение».
---

# Image Generation

## Инструмент

```
generate_image(prompt, size?, quality?, model?)
```

| Параметр | Значения | По умолчанию |
|----------|----------|--------------|
| `prompt` | Текстовое описание (английский даёт лучший результат) | обязательный |
| `size` | `1024x1024` · `1792x1024` · `1024x1792` · `512x512` | `1024x1024` |
| `quality` | `standard` (быстро) · `high` (28 шагов, медленнее) | `standard` |
| `model` | опционально: конкретная модель провайдера | авто |

---

## Как выбрать размер

| Тип контента | Размер |
|---|---|
| Пейзаж, интерьер, сцена вширь, баннер, обои рабочего стола | `1792x1024` |
| Портрет, постер, обложка книги, вертикальный баннер | `1024x1792` |
| Логотип, иконка, аватар, квадратный арт | `1024x1024` |
| Быстрый черновик / проверка идеи | `512x512` |

---

## Workflow: черновик → финал

Для нетривиальных запросов экономь время — сначала проверь идею:

1. **Черновик** — `size=512x512`, `quality=standard`. Быстро, дёшево. Проверяешь концепцию.
2. **Финал** — `size=1024x1024` или больше, `quality=high`. Когда направление одобрено.

Сообщи пользователю, что генеришь черновик, и предложи улучшить после просмотра.

---

## Структура хорошего промпта

Качество результата напрямую зависит от промпта. Строй его по этой схеме:

```
[Subject] [Action/Pose], [Setting/Background], [Style], [Lighting], [Color palette], [Camera/Composition], [Details to avoid]
```

**Элементы (добавляй по необходимости):**

- **Subject** — кто/что главное: `a young woman reading`, `a futuristic city`, `a golden retriever`
- **Style** — `photorealistic`, `oil painting`, `watercolor`, `anime`, `digital art`, `3D render`, `minimalist illustration`, `cinematic`, `sketch`
- **Lighting** — `golden hour light`, `soft studio lighting`, `dramatic side lighting`, `neon glow`, `overcast daylight`
- **Composition** — `close-up portrait`, `wide establishing shot`, `bird's eye view`, `rule of thirds`, `centered symmetry`
- **Color palette** — `warm earth tones`, `cool blues and purples`, `monochrome`, `vibrant saturated colors`
- **Negative hints** — `no text`, `no watermark`, `no blur`, `no extra limbs`

**Пример хорошего промпта:**
```
A lone astronaut standing on the surface of Mars, looking at Earth rising above the horizon, 
photorealistic, dramatic cinematic lighting, orange-red dust and rocks in foreground, 
deep blue Earth in distance, ultra-detailed spacesuit, wide establishing shot, 
volumetric atmosphere, no text, no watermark
```

---

## Правила промпта

- **Пиши на английском** — все провайдеры (FLUX, Stable Diffusion, DALL-E) лучше понимают английский. Перводи запрос пользователя сам.
- **Конкретность > абстрактность** — «a red sports car parked in front of a modern glass building at dusk» лучше, чем «красивая машина».
- **Стиль всегда указывай** — без явного стиля результат непредсказуем. Если пользователь не уточнил — выбери подходящий (photorealistic для реалистичных сцен, digital art для игрового/фэнтези контента).
- **Не перегружай** — 50-100 слов оптимально. Длиннее — провайдер может игнорировать часть.

---

## Итерация и улучшение

После показа изображения пользователю:
- Если результат не устраивает — спроси, что именно изменить (стиль? цвета? состав? детали?)
- Перегенерируй с уточнённым промптом, не спрашивай разрешения лишний раз
- Для style transfer / редактирования — описывай изменения явно в промпте

---

## Частые сценарии

**Аватар/портрет персонажа:**
```
Portrait of [description], facing camera, [style], professional headshot composition,
clean background, high detail on face, soft lighting
```

**Логотип/иконка:**
```
Minimalist flat logo design for [concept], simple geometric shapes, 
[color1] and [color2] palette, white background, vector style, clean lines
```

**Иллюстрация к тексту:**
```
Illustration for a story about [topic], [mood] atmosphere, [style], 
[key visual elements from the text], no text overlay
```

**Концепт-арт продукта/UI:**
```
Product concept render of [description], clean studio shot, 
3D render, professional product photography lighting, white background
```

**Фон/обои:**
```
[Scene description], desktop wallpaper, ultra wide 1792x1024, 
[style], [mood], no subjects in center (leave space for icons)
```
