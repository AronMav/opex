# <handler>
#   <id>transcribe</id>
#   <label lang="ru">Транскрибировать</label>
#   <label lang="en">Transcribe</label>
#   <description lang="ru">Речь из аудио/видео в текст</description>
#   <description lang="en">Speech from audio/video to text</description>
#   <icon>mic</icon>
#   <match>
#     <mime>audio/*</mime>
#     <mime>video/*</mime>
#     <max_size_mb>200</max_size_mb>
#     <domain>youtube.com</domain>
#     <domain>youtu.be</domain>
#   </match>
#   <capability>stt</capability>
#   <execution>sync</execution>
#   <output>text</output>
#   <params>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <order>10</order>
#   <enabled>true</enabled>
# </handler>
"""transcribe — speech-to-text via the active STT provider.

R12: the upload bytes arrive on file.bytes; the provider wrapper passes the
shared raw client to the STT backend (a trusted provider endpoint)."""


async def run(ctx, file, params):
    language = params.get("language", "ru")
    text = await ctx.stt.transcribe(
        file.bytes, filename=file.filename, language=language
    )
    return ctx.result.text(text)
