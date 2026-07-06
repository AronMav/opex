"""Exec-free validation of a handler source: descriptor parse + Python syntax
+ presence of a top-level `run` function — WITHOUT importing/executing the
module (never runs untrusted top-level code)."""

from __future__ import annotations

import ast

from handlers.descriptor import DescriptorError, parse_descriptor


def validate_source(source: str, expected_id: str | None = None) -> dict:
    errors: list[dict] = []
    descriptor: dict | None = None

    # 1. descriptor block (fail-closed parse; no exec)
    try:
        d = parse_descriptor(source, "workspace")
        descriptor = {
            "id": d.id,
            "labels": d.labels,
            "descriptions": d.descriptions,
            "icon": d.icon,
            "match": {"mime": d.match_mimes, "domains": d.match_domains, "max_size_mb": d.max_size_mb},
            "capability": d.capability,
            "execution": d.execution,
            "output": d.output,
            "params": d.params,
            "order": d.order,
            "enabled": d.enabled,
        }
        if expected_id is not None and d.id != expected_id:
            errors.append({
                "field": "id",
                "message": f"descriptor id '{d.id}' must match handler id '{expected_id}'",
            })
    except DescriptorError as e:
        errors.append({"field": "descriptor", "message": str(e)})

    # 2. Python syntax — parse only, never execute.
    tree: ast.Module | None = None
    try:
        tree = ast.parse(source)
    except SyntaxError as e:
        errors.append({"field": "python", "message": f"syntax error: {e}"})

    # 3. a top-level `run` function must exist (async def or def).
    if tree is not None:
        has_run = any(
            isinstance(n, (ast.AsyncFunctionDef, ast.FunctionDef)) and n.name == "run"
            for n in tree.body
        )
        if not has_run:
            errors.append({"field": "python", "message": "no top-level `run` function defined"})

    return {"ok": not errors, "descriptor": descriptor, "errors": errors}
