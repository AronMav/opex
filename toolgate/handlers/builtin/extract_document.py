# <handler>
#   <id>extract_document</id>
#   <label lang="ru">Извлечь текст</label>
#   <label lang="en">Extract text</label>
#   <description lang="ru">Текст из PDF/DOCX/текстовых файлов</description>
#   <description lang="en">Text from PDF/DOCX/text files</description>
#   <icon>file-text</icon>
#   <match>
#     <mime>application/pdf</mime>
#     <mime>application/vnd.openxmlformats-officedocument.wordprocessingml.document</mime>
#     <mime>application/msword</mime>
#     <mime>application/json</mime>
#     <mime>application/xml</mime>
#     <mime>application/yaml</mime>
#     <mime>application/x-yaml</mime>
#     <mime>application/x-json</mime>
#     <mime>text/*</mime>
#     <max_size_mb>50</max_size_mb>
#   </match>
#   <execution>sync</execution>
#   <output>text</output>
#   <params>
#     <param name="max_chars" type="int" default="8000" required="false"/>
#   </params>
#   <config>
#     <field name="max_chars" type="int" default="8000" label="Макс. символов" description="Ограничение объёма извлечённого текста по умолчанию (0 = без лимита)"/>
#   </config>
#   <order>20</order>
#   <enabled>true</enabled>
# </handler>
"""extract_document — text extraction parsed IN-PROCESS from file.bytes (R12).

PDF via pymupdf (fitz), DOCX via python-docx, everything text/* (and unknown)
via best-effort UTF-8 decode. The blocking CPU parse runs in a worker thread
via asyncio.to_thread (R5 CPU-offload). NO loopback /extract-text-url POST —
toolgate's SSRF guard blocks loopback and core already handed us the bytes."""

import asyncio
import io

import fitz  # pymupdf
import docx

_DOCX_MIMES = {
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/msword",
}


def _extract_sync(data: bytes, mime: str) -> str:
    if mime == "application/pdf":
        parts = []
        with fitz.open(stream=data, filetype="pdf") as doc:
            for page in doc:
                parts.append(page.get_text())
        return "\n".join(parts)
    if mime in _DOCX_MIMES:
        document = docx.Document(io.BytesIO(data))
        return "\n".join(p.text for p in document.paragraphs)
    # text/* and unknown -> best-effort decode
    return data.decode("utf-8", errors="replace")


def _int_config(config: dict, key: str, fallback: int) -> int:
    """Read an int-valued config field (UI stores values as strings)."""
    try:
        v = config.get(key)
        return int(v) if v not in (None, "") else fallback
    except (TypeError, ValueError):
        return fallback


async def run(ctx, file, params):
    # Per-agent operator default (valve); an explicit per-call param still wins.
    default_max = _int_config(ctx.config, "max_chars", 8000)
    max_chars = int(params.get("max_chars", default_max))
    try:
        text = await asyncio.to_thread(_extract_sync, file.bytes, file.mime)
    except Exception as e:  # corrupt/unsupported document
        return ctx.result.failed(f"extract failed: {e}")
    if max_chars > 0:
        text = text[:max_chars]
    return ctx.result.text(text)
