"""Scans builtin + workspace handler files, parses their XML descriptor,
imports the module, and captures `run`. Every per-file load is wrapped in
try/except so a bad workspace file is skipped+logged, never aborting the scan.
Builtin ids are reserved: a workspace file reusing one is rejected (builtin
wins)."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import logging
import os
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

from handlers.descriptor import HandlerDescriptor, DescriptorError, parse_descriptor

log = logging.getLogger("toolgate.handlers")


@dataclass
class LoadedHandler:
    descriptor: HandlerDescriptor
    run: Callable
    tier: str


def _read_source(path: str) -> str:
    return Path(path).read_text(encoding="utf-8")


def _import_run(path: str):
    """Import a handler module from `path` and return its `run` coroutine fn."""
    mod_name = f"_handler_{uuid.uuid4().hex}"
    spec = importlib.util.spec_from_file_location(mod_name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f"cannot create import spec for {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    run = getattr(module, "run", None)
    if run is None or not callable(run):
        raise ImportError(f"{path} has no callable `run`")
    return run


class HandlerRegistry:
    def __init__(self) -> None:
        self._handlers: dict[str, LoadedHandler] = {}

    def load_all(self, builtin_dir: str, workspace_dir: str | None) -> None:
        self._handlers = {}
        # Builtin tier FIRST — its ids become reserved.
        self._scan_dir(builtin_dir, "builtin")
        if workspace_dir:
            ws = os.path.join(workspace_dir, "file_handlers")
            self._scan_dir(ws, "workspace")

    def _scan_dir(self, directory: str, tier: str) -> None:
        if not directory or not os.path.isdir(directory):
            return
        for name in sorted(os.listdir(directory)):
            if not name.endswith(".py") or name.startswith("_"):
                continue
            self._load_one(os.path.join(directory, name), tier)

    def _load_one(self, path: str, tier: str) -> None:
        try:
            source = _read_source(path)
            descriptor = parse_descriptor(source, tier)
            if descriptor.id in self._handlers:
                existing = self._handlers[descriptor.id]
                log.warning(
                    "handler id %r in %s clashes with existing %s handler - rejected",
                    descriptor.id, path, existing.tier,
                )
                return
            run = _import_run(path)
            self._handlers[descriptor.id] = LoadedHandler(descriptor, run, tier)
            log.info("loaded handler %s (tier=%s)", descriptor.id, tier)
        except DescriptorError as e:
            log.warning("skipping handler file %s: descriptor error: %s", path, e)
        except (SyntaxError, ImportError) as e:
            log.warning("skipping handler file %s: import error: %s", path, e)
        except Exception as e:
            log.warning("skipping handler file %s: unexpected error: %s", path, e)

    def get(self, handler_id: str) -> LoadedHandler | None:
        return self._handlers.get(handler_id)

    def reload_file(self, path: str) -> None:
        """Reload a single workspace file in place (hot-reload). Builtin-id
        clashes are still rejected by _load_one. A deleted file is a no-op
        (the previously loaded handler stays until the next full load_all)."""
        if not os.path.isfile(path):
            return
        self._load_one(path, "workspace")

    def manifests(self) -> list[dict]:
        items = [self._manifest(h) for h in self._handlers.values()]
        items.sort(key=lambda m: (m["order"], m["id"]))
        return items

    def _manifest(self, h: LoadedHandler) -> dict:
        d = h.descriptor
        return {
            "id": d.id,
            "labels": d.labels,
            "descriptions": d.descriptions,
            "icon": d.icon,
            "match": {"mime": d.match_mimes, "max_size_mb": d.max_size_mb},
            "capability": d.capability,
            "provider": None,  # filled by the router from the active provider (R5)
            "execution": d.execution,
            "output": d.output,
            "params": d.params,
            "order": d.order,
            "tier": h.tier,
        }

    def etag(self) -> str:
        canonical = json.dumps(self.manifests(), sort_keys=True,
                               ensure_ascii=False).encode("utf-8")
        return '"' + hashlib.sha256(canonical).hexdigest() + '"'
