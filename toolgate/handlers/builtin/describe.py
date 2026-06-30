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
#   <order>10</order>
#   <enabled>true</enabled>
# </handler>
"""describe — image description via the active vision provider.

R12: the upload bytes arrive on file.bytes; the provider wrapper passes the
shared raw client to the vision backend (a trusted provider endpoint)."""

from helpers import default_vision_prompt


async def run(ctx, file, params):
    prompt = (params.get("prompt") or "").strip()
    language = params.get("language", "ru")
    if not prompt:
        prompt = default_vision_prompt(language)
    text = await ctx.vision.describe(
        file.bytes, content_type=file.mime, prompt=prompt
    )
    return ctx.result.text(text)
