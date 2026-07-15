"""Shared transcript helpers (pure Python, no external deps).

`strip_transcript_timecodes` strips leading `[MM:SS]` markers from a
line-oriented transcript so the LLM (or the end user, via `/transcribe`)
sees clean text with no timecodes. Moved out of
`handlers/builtin/summarize_video.py` so it can be shared with
`term_fixer.py` and `routers/stt.py` without an import cycle through the
handler module.
"""
from typing import Optional


def _parse_line_minute(line: str) -> Optional[int]:
    """Parse `[MM:SS]` or `[MMM:SS]` prefix → whole minutes. None if absent."""
    stripped = line.lstrip()
    if not stripped.startswith("["):
        return None
    close = stripped.find("]")
    if close < 0:
        return None
    inner = stripped[1:close]
    if ":" not in inner:
        return None
    m_str, s_str = inner.split(":", 1)
    if not m_str or not m_str.isdigit():
        return None
    if len(s_str) != 2 or not s_str.isdigit():
        return None
    return int(m_str)


def strip_transcript_timecodes(text: str) -> str:
    """Strip leading `[MM:SS]` markers so the LLM sees clean text (no timecodes)."""
    result: list[str] = []
    for line in text.splitlines():
        stripped = line.lstrip()
        m = _parse_line_minute(line)
        if m is not None:
            # Find the closing ] and take everything after it
            close = stripped.find("]")
            after = stripped[close + 1:].lstrip()
            result.append(after)
        else:
            result.append(line)
    return "\n".join(result)
