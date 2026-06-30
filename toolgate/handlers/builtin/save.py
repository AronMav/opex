# <handler>
#   <id>save</id>
#   <label lang="ru">Сохранить</label>
#   <label lang="en">Save</label>
#   <description lang="ru">Сохранить файл как есть</description>
#   <description lang="en">Keep the file as-is</description>
#   <icon>save</icon>
#   <match>
#     <mime>*/*</mime>
#   </match>
#   <execution>sync</execution>
#   <output>file</output>
#   <order>1</order>
#   <enabled>true</enabled>
# </handler>
"""save — keep the uploaded file as a persisted artifact (no processing).

The bytes are already persisted in core uploads (core downloaded them in Rust
and POSTed the multipart). This handler just confirms persistence; the core
records a file-derived message referencing the original upload."""

from handlers.context import HandlerResult


async def run(ctx, file, params):
    return HandlerResult(
        status="ok",
        summary_text=f"Saved {file.filename} ({file.size} bytes)",
        artifact_urls=[],
    )
