"""HandlerDescriptor dataclass + XML descriptor parser/validator.

Single source of truth for the handler schema. One handler = one .py file
whose leading "# <handler> ... # </handler>" comment block describes it.
"""

from __future__ import annotations

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
    max_size_mb: int | None
    capability: str | None
    execution: str  # "sync" | "async"
    output: str  # "text" | "file" | "card"
    params: list[dict]
    order: int
    enabled: bool
    tier: str  # "builtin" | "workspace"
