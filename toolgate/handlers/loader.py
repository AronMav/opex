"""Scans builtin + workspace handler files, parses their XML descriptor,
imports the module, and captures `run`. Every per-file load is wrapped in
try/except so a bad workspace file is skipped+logged, never aborting the scan.
A workspace file whose id matches a builtin OVERRIDES (shadows) the builtin;
deleting the override resurfaces the pristine builtin (reset-to-default)."""

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
        # Pristine builtins kept separately so an override can be reverted
        # (reset-to-default) by resurfacing the builtin.
        self._builtins: dict[str, LoadedHandler] = {}
        # Maps normalized absolute path → workspace handler id registered from it.
        self._path_to_id: dict[str, str] = {}

    @staticmethod
    def _norm(path: str) -> str:
        """Return a canonical, case-folded absolute path for use as a map key."""
        return os.path.normcase(os.path.abspath(path))

    def load_all(self, builtin_dir: str, workspace_dir: str | None) -> None:
        self._handlers = {}
        self._builtins = {}
        self._path_to_id = {}
        # Builtin tier FIRST — retained in _builtins AND seeded as effective.
        self._scan_dir(builtin_dir, "builtin")
        self._builtins = dict(self._handlers)
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
            if tier == "workspace":
                # Collision rules: a workspace id matching a BUILTIN id is an
                # allowed OVERRIDE (shadows the builtin). A workspace id matching
                # another WORKSPACE handler from a different path is rejected.
                existing = self._handlers.get(descriptor.id)
                if existing is not None and existing.tier == "workspace":
                    log.warning(
                        "handler id %r in %s clashes with existing workspace handler - rejected",
                        descriptor.id, path,
                    )
                    return
            run = _import_run(path)
            self._handlers[descriptor.id] = LoadedHandler(descriptor, run, tier)
            if tier == "workspace":
                self._path_to_id[self._norm(path)] = descriptor.id
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
        """Upsert a workspace handler from *path* (hot-reload on MODIFY events).

        - Evicts any previously-registered workspace handler from this path
          first, so same-id edits and id-renames both work correctly.
        - A workspace id matching a builtin id is now an OVERRIDE (shadows the
          builtin). If the old id was an override of a builtin, resurfaces the
          builtin before loading the new version.
        - A parse/import error after eviction leaves NO entry for the path —
          the stale version is not retained; a broken file disappears from the
          registry rather than serving stale data.
        - A missing file (DELETE was processed by remove_file instead) is a
          no-op here.
        """
        if not os.path.isfile(path):
            return
        norm = self._norm(path)
        old_id = self._path_to_id.pop(norm, None)
        if old_id is not None and old_id in self._handlers and self._handlers[old_id].tier == "workspace":
            del self._handlers[old_id]
            # If this was an override of a builtin, resurface the builtin.
            if old_id in self._builtins:
                self._handlers[old_id] = self._builtins[old_id]
        self._load_one(path, "workspace")

    def remove_file(self, path: str) -> None:
        """Evict the workspace handler registered from *path* (DELETE events).

        No-op if the path was not previously registered as a workspace handler.
        Override removed → resurfaces the pristine builtin (reset-to-default).
        """
        norm = self._norm(path)
        old_id = self._path_to_id.pop(norm, None)
        if old_id is not None and old_id in self._handlers and self._handlers[old_id].tier == "workspace":
            del self._handlers[old_id]
            # Override removed → resurface the pristine builtin (reset-to-default).
            if old_id in self._builtins:
                self._handlers[old_id] = self._builtins[old_id]
                log.info("reset handler %r to builtin default (override removed)", old_id)
            else:
                log.info("removed workspace handler %r (file deleted)", old_id)

    def manifests(self) -> list[dict]:
        items = [self._manifest(h) for h in self._handlers.values()]
        items.sort(key=lambda m: (m["order"], m["id"]))
        return items

    def _manifest(self, h: LoadedHandler) -> dict:
        d = h.descriptor
        is_builtin_id = d.id in self._builtins
        overridden = is_builtin_id and h.tier == "workspace"
        source = "override" if overridden else ("builtin" if is_builtin_id else "workspace")
        tier = "builtin" if is_builtin_id else "workspace"
        return {
            "id": d.id,
            "labels": d.labels,
            "descriptions": d.descriptions,
            "icon": d.icon,
            "match": {"mime": d.match_mimes, "domains": d.match_domains, "max_size_mb": d.max_size_mb},
            "capability": d.capability,
            "provider": None,  # filled by the router from the active provider (R5)
            "execution": d.execution,
            "output": d.output,
            "params": d.params,
            "order": d.order,
            "tier": tier,
            "source": source,
        }

    def etag(self) -> str:
        canonical = json.dumps(self.manifests(), sort_keys=True,
                               ensure_ascii=False).encode("utf-8")
        return '"' + hashlib.sha256(canonical).hexdigest() + '"'
