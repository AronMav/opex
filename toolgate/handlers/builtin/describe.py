# <handler>
#   <id>describe</id>
#   <label lang="ru">Описать</label>
#   <label lang="en">Describe</label>
#   <description lang="ru">Описание изображения</description>
#   <description lang="en">Image description</description>
#   <icon>image</icon>
#   <match>
#     <mime>image/*</mime>
#     <max_size_mb>20</max_size_mb>
#   </match>
#   <capability>vision</capability>
#   <execution>sync</execution>
#   <output>text</output>
#   <params>
#     <param name="prompt" type="string" default="" required="false"/>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <config>
#     <field name="default_prompt" type="string" default="" label="Промпт по умолчанию" description="Инструкция для vision-модели, когда модель не задала свой промпт (пусто = встроенный промпт)"/>
#     <field name="max_tokens" type="int" default="2000" label="Макс. токенов ответа" description="Ограничение длины описания"/>
#   </config>
#   <order>10</order>
#   <enabled>true</enabled>
# </handler>
"""describe — image description via the active vision provider.

R12: the upload bytes arrive on file.bytes; the provider wrapper passes the
shared raw client to the vision backend (a trusted provider endpoint)."""

from helpers import default_vision_prompt


def _int_config(config: dict, key: str, fallback: int) -> int:
    """Read an int-valued config field (values arrive as strings from the UI)."""
    try:
        v = config.get(key)
        return int(v) if v not in (None, "") else fallback
    except (TypeError, ValueError):
        return fallback


async def run(ctx, file, params):
    prompt = (params.get("prompt") or "").strip()
    language = params.get("language", "ru")
    if not prompt:
        # Operator default prompt (valve) → built-in default.
        prompt = (ctx.config.get("default_prompt") or "").strip() or default_vision_prompt(language)
    max_tokens = _int_config(ctx.config, "max_tokens", 2000)
    text = await ctx.vision.describe(
        file.bytes, content_type=file.mime, prompt=prompt, max_tokens=max_tokens
    )
    return ctx.result.text(text)
