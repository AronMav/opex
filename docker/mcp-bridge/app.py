"""Generic stdio-to-HTTP bridge for MCP servers.

Reads MCP_COMMAND env var (JSON array) and spawns the process for each request.
Handles the MCP initialize handshake automatically.
"""

import asyncio
import json
import os

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse

app = FastAPI()
COMMAND = json.loads(os.environ.get("MCP_COMMAND", '["echo","no MCP_COMMAND configured"]'))


def _infer_working_dir(command):
    """Return the MCP server's intended root directory, if any.

    Filesystem MCP servers (e.g. `mcp/filesystem`) receive the allowed
    directory as their last argument, and relative paths in tool calls are
    resolved against the subprocess working directory. Git MCP servers
    receive `--repository /src`. If the last command token is an absolute
    path, use it as cwd so relative paths land inside the container mount
    instead of the bridge's own `/bridge` directory.
    """
    if not command:
        return None
    last = command[-1]
    if isinstance(last, str) and last.startswith("/") and last not in ("/", "/bridge"):
        return last
    return None


async def stdio_call(method: str, params: dict, req_id):
    """Spawn MCP subprocess, run the initialize handshake, and return the
    response line matching `req_id`.

    We stream stdin and then read stdout line-by-line until the matching
    response arrives — instead of `communicate()`, which closes stdin and
    waits for the process to exit before returning any output. An async MCP
    tool (e.g. `fetch`, which performs network I/O inside `tools/call`) often
    exits on stdin EOF BEFORE its coroutine finishes writing the response, so
    `communicate()` returned only the synchronous `initialize` reply and
    silently dropped the actual call — surfacing as
    "No valid JSON response from MCP" with empty stderr. Keeping stdin open
    until we've read our response lets the async handler complete.
    """
    messages = [
        {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "bridge", "version": "1.0"},
        }},
        {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
        {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params or {}},
    ]
    stdin_data = ("\n".join(json.dumps(m) for m in messages) + "\n").encode()

    cwd = _infer_working_dir(COMMAND)
    kwargs = {}
    if cwd is not None:
        kwargs["cwd"] = cwd

    proc = await asyncio.create_subprocess_exec(
        *COMMAND,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        **kwargs,
    )

    async def _write_then_read():
        try:
            proc.stdin.write(stdin_data)
            await proc.stdin.drain()
        except (BrokenPipeError, ConnectionResetError):
            # Process died before consuming stdin; the readline below hits EOF.
            pass
        # Do NOT close stdin — keep the process alive so an async handler can
        # finish its I/O and emit the response.
        while True:
            line = await proc.stdout.readline()
            if not line:
                return None  # stdout EOF before a matching response
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            if obj.get("id") == req_id:
                return obj

    timed_out = False
    try:
        result = await asyncio.wait_for(_write_then_read(), timeout=30)
    except asyncio.TimeoutError:
        result = None
        timed_out = True
    finally:
        try:
            proc.kill()
        except ProcessLookupError:
            pass
        try:
            err_bytes = await asyncio.wait_for(proc.stderr.read(), timeout=2)
        except (asyncio.TimeoutError, Exception):
            err_bytes = b""
        await proc.wait()

    if result is not None:
        return result
    err = err_bytes.decode("utf-8", errors="replace")[:500]
    if timed_out:
        raise RuntimeError(f"MCP process timeout (30s). stderr: {err}")
    raise RuntimeError(f"No valid JSON response from MCP. stderr: {err}")


@app.get("/health")
async def health():
    return {"ok": True}


@app.post("/mcp")
async def mcp_endpoint(request: Request):
    body = await request.json()
    try:
        result = await stdio_call(
            body.get("method", ""),
            body.get("params", {}),
            body.get("id", 1),
        )
        return JSONResponse(result)
    except Exception as e:
        return JSONResponse({
            "jsonrpc": "2.0",
            "error": {"code": -32000, "message": str(e)},
            "id": body.get("id", 1),
        })
