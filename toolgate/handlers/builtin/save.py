# <handler>
#   <id>save</id>
#   <label lang="ru">Сохранить</label>
#   <label lang="en">Save</label>
#   <description lang="ru">Сохранить файл в workspace</description>
#   <description lang="en">Save file to workspace</description>
#   <icon>save</icon>
#   <match>
#     <mime>*/*</mime>
#   </match>
#   <execution>sync</execution>
#   <output>file</output>
#   <params>
#     <param name="path" type="string" required="false" description="Relative path in workspace (e.g. 'uploads/file.json'). If empty, the original filename is used in the workspace root."/>
#   </params>
#   <order>1</order>
#   <enabled>true</enabled>
# </handler>
"""save — persist the uploaded file to the workspace filesystem.

The uploaded bytes are POSTed from core (multipart "file" field). This
handler writes them to the workspace directory at the given relative path
(from params.path, or the original filename if path is empty). The file
then becomes accessible to agents via workspace_read and to the operator
via the workspace file browser.

The path is validated to be relative (no leading /, no .. traversal) —
toolgate runs with workspace access only, so this is a safety net, not
the primary guard (core's workspace path validation is the real boundary).
"""

import os
from pathlib import Path

from handlers.context import HandlerResult

# Workspace root is injected by core via the WORKSPACE_DIR env var.
# Core sets it to "../workspace" (relative to toolgate's working_dir,
# which is ~/opex/toolgate → resolves to ~/opex/workspace). The fallback
# handles test/standalone runs where the env var is not set.
WORKSPACE_DIR = os.environ.get("WORKSPACE_DIR", "../workspace")


def _safe_rel_path(raw: str, fallback: str) -> str:
    """Sanitize a user-supplied relative path. Rejects absolute paths,
    parent-dir traversal, and empty segments. Falls back to the original
    filename when the path is unusable."""
    raw = (raw or "").strip()
    if not raw:
        raw = fallback
    # Normalize: strip leading slashes, convert backslashes
    raw = raw.replace("\\", "/").lstrip("/")
    # Reject traversal
    if ".." in raw.split("/"):
        return os.path.basename(fallback) or "saved_file"
    return raw


async def run(ctx, file, params):
    rel_path = _safe_rel_path(
        params.get("path", ""),
        file.filename or "saved_file",
    )
    target = Path(WORKSPACE_DIR) / rel_path
    # Create parent dirs
    target.parent.mkdir(parents=True, exist_ok=True)
    # Write bytes
    target.write_bytes(file.bytes)
    return HandlerResult(
        status="ok",
        summary_text=f"Saved {file.filename} ({file.size} bytes) to {rel_path}",
        artifact_urls=[],
    )
