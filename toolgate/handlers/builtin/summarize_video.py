# <handler>
#   <id>summarize_video</id>
#   <label lang="ru">Конспект видео</label>
#   <label lang="en">Summarize Video</label>
#   <description lang="ru">Транскрибирует видео и создаёт структурированный конспект</description>
#   <description lang="en">Transcribes video and creates a structured summary</description>
#   <icon>video</icon>
#   <match>
#     <mime>video/*</mime>
#     <max_size_mb>2000</max_size_mb>
#   </match>
#   <capability>stt</capability>
#   <execution>async</execution>
#   <output>text</output>
#   <params>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <order>20</order>
#   <enabled>true</enabled>
# </handler>
"""summarize_video — async handler that transcribes a video file and produces a
structured text summary.

R12: bytes arrive via ctx.file.bytes (written by the router to a tempfile and
read by the runner — never fetched over loopback). The STT provider wrapper
handles the transcription call via the trusted provider backend.

This handler is async (execution=async in the descriptor) so the router returns
202 immediately and the out-of-process runner (handlers/runner.py) executes it
without blocking the toolgate HTTP event loop (toolgate uses --workers 1).
"""


async def run(ctx, file, params):
    language = params.get("language", "ru")

    await ctx.progress("transcribe", 10)
    transcript = await ctx.stt.transcribe(
        file.bytes,
        filename=file.filename or "video.mp4",
        language=language,
    )

    await ctx.progress("digest", 60)
    # Return the raw transcript as summary text. A richer digesting step
    # (LLM summarize) is added in Task 5; here we return the transcript
    # so the handler is functional end-to-end for Phase 5 routing tests.
    return ctx.result.text(transcript)
