"""HandlerDescriptor dataclass + XML descriptor parser/validator.

Single source of truth for the handler schema. One handler = one .py file
whose leading "# <handler> ... # </handler>" comment block describes it.
"""

from __future__ import annotations

import re

import defusedxml.ElementTree as ET
from dataclasses import dataclass


class DescriptorError(Exception):
    """Raised when a handler descriptor block is missing, malformed, or invalid."""


@dataclass
class HandlerDescriptor:
    id: str
    labels: dict[str, str]
    descriptions: dict[str, str]
    icon: str
    match_mimes: list[str]
    match_domains: list[str]  # youtube.com, youtu.be, etc.
    max_size_mb: int | None
    capability: str | None
    execution: str  # "sync" | "async"
    output: str  # "text" | "file" | "card"
    params: list[dict]
    # Operator-configurable settings (OpenWebUI-style "valves"), distinct from
    # `params` (which the model fills per call). Each: {name, type, default,
    # label, description}. Values are set per-agent in the tool settings UI and
    # injected as `ctx.config` at run time.
    config: list[dict]
    order: int
    enabled: bool
    tier: str  # "builtin" | "workspace"


# ── Parser ──────────────────────────────────────────────────────────────────

_BLOCK_RE = re.compile(r"#\s*<handler>(.*?)#\s*</handler>", re.DOTALL)
_ID_RE = re.compile(r"^[a-z0-9_-]+$")
_VALID_EXECUTION = {"sync", "async"}


def _extract_block(source: str) -> str:
    """Pull the leading '# <handler> ... # </handler>' comment block out of a
    handler source file and strip the leading '# ' from each line so the
    remainder is valid XML."""
    m = _BLOCK_RE.search(source)
    if not m:
        raise DescriptorError("no <handler> descriptor block found")
    inner = m.group(0)
    lines = []
    for line in inner.splitlines():
        stripped = line.lstrip()
        if not stripped.startswith("#"):
            continue
        # remove the leading '#' and a single following space if present
        body = stripped[1:]
        if body.startswith(" "):
            body = body[1:]
        lines.append(body)
    return "\n".join(lines)


def _text(el: ET.Element, tag: str, default: str | None = None) -> str | None:
    child = el.find(tag)
    if child is None or child.text is None:
        return default
    return child.text.strip()


def parse_descriptor(source: str, tier: str) -> HandlerDescriptor:
    """Parse a handler source file's descriptor block into a validated
    HandlerDescriptor. Raises DescriptorError on any structural or validation
    failure (fail-closed)."""
    xml_str = _extract_block(source)
    try:
        root = ET.fromstring(xml_str)
    except ET.ParseError as e:
        raise DescriptorError(f"malformed descriptor XML: {e}") from e

    labels: dict[str, str] = {}
    for el in root.findall("label"):
        lang = el.get("lang")
        if lang and el.text:
            labels[lang] = el.text.strip()

    descriptions: dict[str, str] = {}
    for el in root.findall("description"):
        lang = el.get("lang")
        if lang and el.text:
            descriptions[lang] = el.text.strip()

    match_el = root.find("match")
    match_mimes: list[str] = []
    match_domains: list[str] = []
    max_size_mb: int | None = None
    if match_el is not None:
        for m in match_el.findall("mime"):
            if m.text:
                match_mimes.append(m.text.strip())
        for d in match_el.findall("domain"):
            if d.text:
                match_domains.append(d.text.strip().lower())
        size_txt = _text(match_el, "max_size_mb")
        if size_txt is not None:
            try:
                max_size_mb = int(size_txt)
            except ValueError as e:
                raise DescriptorError(
                    f"descriptor max_size_mb must be an integer, got '{size_txt}'"
                ) from e

    params: list[dict] = []
    params_el = root.find("params")
    if params_el is not None:
        for p in params_el.findall("param"):
            params.append(
                {
                    "name": p.get("name", ""),
                    "type": p.get("type", "string"),
                    "default": p.get("default"),
                    "required": p.get("required", "false").strip().lower() == "true",
                }
            )

    # Operator-configurable settings block (optional).
    config: list[dict] = []
    config_el = root.find("config")
    if config_el is not None:
        for f in config_el.findall("field"):
            name = f.get("name", "")
            config.append(
                {
                    "name": name,
                    "type": f.get("type", "string"),
                    "default": f.get("default"),
                    "label": f.get("label") or name,
                    "description": f.get("description", ""),
                }
            )

    order_txt = _text(root, "order")
    # F118: guard the <order> cast (mirrors max_size_mb above). An unguarded
    # int() on a non-numeric value raised an uncaught ValueError that surfaced as
    # a 500 from POST /handlers/validate instead of a structured field error.
    if order_txt is not None:
        try:
            order = int(order_txt)
        except ValueError as e:
            raise DescriptorError(
                f"descriptor order must be an integer, got '{order_txt}'"
            ) from e
    else:
        order = 100
    enabled_txt = _text(root, "enabled")

    hid = (_text(root, "id") or "").strip()
    execution = (_text(root, "execution") or "").strip()

    if not hid:
        raise DescriptorError("descriptor missing required <id>")
    if not _ID_RE.match(hid):
        raise DescriptorError(
            f"descriptor id '{hid}' must match ^[a-z0-9_-]+$"
        )
    if not labels:
        raise DescriptorError(f"descriptor '{hid}' missing required <label>")
    if not match_mimes and not match_domains:
        raise DescriptorError(
            f"descriptor '{hid}' must declare at least one <mime> or <domain>"
        )
    if execution not in _VALID_EXECUTION:
        raise DescriptorError(
            f"descriptor '{hid}' execution must be 'sync' or 'async', got '{execution}'"
        )

    return HandlerDescriptor(
        id=hid,
        labels=labels,
        descriptions=descriptions,
        icon=_text(root, "icon", "file") or "file",
        match_mimes=match_mimes,
        match_domains=match_domains,
        max_size_mb=max_size_mb,
        capability=_text(root, "capability"),
        execution=execution,
        output=_text(root, "output", "text") or "text",
        params=params,
        config=config,
        order=order,
        enabled=(enabled_txt is None) or enabled_txt.strip().lower() == "true",
        tier=tier,
    )
