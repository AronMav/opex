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
#     <mime>text/*</mime>
#     <max_size_mb>50</max_size_mb>
#   </match>
#   <execution>sync</execution>
#   <output>text</output>
#   <params>
#     <param name="max_chars" type="int" default="8000" required="false"/>
#   </params>
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


async def run(ctx, file, params):
    max_chars = int(params.get("max_chars", 8000))
    try:
        text = await asyncio.to_thread(_extract_sync, file.bytes, file.mime)
    except Exception as e:  # corrupt/unsupported document
        return ctx.result.failed(f"extract failed: {e}")
    if max_chars > 0:
        text = text[:max_chars]
    return ctx.result.text(text)
