# <handler>
#   <id>transcribe</id>
#   <label lang="ru">Транскрибировать</label>
#   <label lang="en">Transcribe</label>
#   <description lang="ru">Речь из аудио/видео в текст (без конспекта; названия могут быть автоисправлены)</description>
#   <description lang="en">Speech from audio/video to text (raw, no summary; product names may be auto-corrected)</description>
#   <icon>mic</icon>
#   <match>
#     <mime>audio/*</mime>
#     <mime>video/*</mime>
#     <max_size_mb>2000</max_size_mb>
#     <domain>youtube.com</domain>
#     <domain>youtu.be</domain>
#     <domain>yadi.sk</domain>
#     <domain>disk.yandex.ru</domain>
#     <domain>disk.yandex.com</domain>
#     <domain>disk.yandex.kz</domain>
#     <domain>disk.yandex.by</domain>
#     <domain>disk.yandex.uz</domain>
#   </match>
#   <capability>stt</capability>
#   <execution>async</execution>
#   <output>text</output>
#   <params>
#     <param name="language" type="string" default="ru" required="false"/>
#   </params>
#   <config>
#     <field name="default_language" type="string" default="ru" label="Язык по умолчанию" description="Язык распознавания, если модель не указала его явно (ru, en, auto, …)"/>
#     <field name="fix_terms" type="bool" default="true" label="Исправлять названия" description="Определять искажённые STT названия (бренды, плагины, термины) и исправлять их через веб-поиск. Транскрипты короче 300 символов пропускаются."/>
#   </config>
#   <order>10</order>
#   <enabled>true</enabled>
# </handler>
"""transcribe — speech-to-text via the active STT provider.

Async so it can also handle URL sources (YouTube / Yandex.Disk): for a link it
downloads the media and extracts audio (same seam as summarize_video), then
transcribes. For an uploaded file it transcribes the bytes directly. Returns the
RAW transcript (no summary) — the lighter alternative to summarize_video.
R12: upload bytes arrive on file.bytes; the provider wrapper passes the shared
raw client to the STT backend (a trusted provider endpoint).
With fix_terms=on the transcript is post-processed (garbled product names
corrected via web search) — not strictly verbatim."""

async def run(ctx, file, params):
    # Model-supplied language wins; otherwise the operator's default valve.
    language = params.get("language") or ctx.config.get("default_language") or "ru"

    await ctx.progress("fetch", 10)
    if file.bytes:
        audio = file.bytes
        filename = file.filename or "audio.ogg"
    elif file.source_url:
        # url-based job: download the media and extract audio (reuse the same
        # video_helpers seam as summarize_video).
        import tempfile as _tf
        import sys as _sys
        tg_root = str(__import__("pathlib").Path(__file__).resolve().parents[3])
        if tg_root not in _sys.path:
            _sys.path.insert(0, tg_root)
        try:
            from video_helpers import download_video, extract_audio  # type: ignore[import]
            with _tf.TemporaryDirectory() as d:
                # audio_only: transcription never needs the video track — grab the
                # audio stream (~10-30× smaller than the full 1080p container).
                path = await download_video(file.source_url, d, audio_only=True)
                audio = await extract_audio(path)
            filename = "audio.ogg"
        except Exception as exc:
            return ctx.result.failed(f"source_url fetch failed: {exc}")
    else:
        return ctx.result.failed("no file bytes or source_url provided")

    await ctx.progress("transcribe", 50)
    text = await ctx.stt.transcribe(audio, filename=filename, language=language)

    # Empty-transcript guard: a music-only video or a language mismatch yields
    # nothing usable — fail loudly instead of delivering an empty message.
    if not text.strip():
        return ctx.result.failed(
            "не удалось распознать речь (пустой транскрипт): возможно, в источнике "
            "нет разборчивой речи (только музыка) или язык распознавания не совпадает"
        )

    glossary = ""
    fix_enabled = (
        str(ctx.config.get("fix_terms") or "true").strip().lower()
        not in ("false", "0", "no")
    )
    if fix_enabled:
        try:
            from term_fixer import fix_terms  # runner puts toolgate root on sys.path
            fx = await fix_terms(ctx, text, language, progress_pcts=(60, 70, 80))
            text, glossary = fx.transcript, fx.glossary_md
        except Exception as exc:  # fix_terms сам fail-soft; это страховка импорта
            ctx.log.warning("transcribe: fix_terms unavailable: %s", exc)

    await ctx.progress("saving", 90)
    # Глоссарий отделён --- , чтобы читающий агент не считал его частью речи.
    return ctx.result.text(text + (f"\n\n---\n{glossary}" if glossary else ""))
