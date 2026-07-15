"""Unit tests for the shared `transcript.strip_transcript_timecodes` helper."""
from transcript import strip_transcript_timecodes


def test_strips_leading_mmss_markers():
    src = "[00:01] Привет\n[01:05] мир"
    assert strip_transcript_timecodes(src) == "Привет\nмир"


def test_line_without_timecode_kept_verbatim():
    assert strip_transcript_timecodes("просто текст") == "просто текст"


def test_empty_and_blank():
    assert strip_transcript_timecodes("") == ""
    assert strip_transcript_timecodes("[00:00]   ").strip() == ""
