"""PostgreSQL — MCP server for read-only database queries.

Provides schema inspection and SELECT-only query execution.
Rejects any DDL/DML statements (INSERT, UPDATE, DELETE, DROP, etc.).
"""

import os
import re
import json
import asyncpg
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse

DATABASE_URL = os.environ.get("DATABASE_URL", "")

app = FastAPI()
pool: asyncpg.Pool = None

_FORBIDDEN = re.compile(
    r"\b(INSERT|UPDATE|DELETE|DROP|CREATE|ALTER|TRUNCATE|GRANT|REVOKE|COPY|EXECUTE|CALL|DO)\b",
    re.IGNORECASE,
)


@app.on_event("startup")
async def startup():
    global pool
    if DATABASE_URL:
        pool = await asyncpg.create_pool(DATABASE_URL, min_size=1, max_size=3)


@app.on_event("shutdown")
async def shutdown():
    if pool:
        await pool.close()


MCP_TOOLS = [
    {
        "name": "list_tables",
        "description": "List all tables and views in the database with their column names and types.",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "query_db",
        "description": (
            "Execute a read-only SQL SELECT query and return results as JSON. "
            "Only SELECT statements are allowed — any DDL or DML is rejected. "
            "Limit results to avoid large payloads. This is the OPEX application "
            "DB: agent definitions live in TOML files, so there is NO 'agents' "
            "table — call list_tables first when unsure of the schema."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "sql": {
                    "type": "string",
                    "description": "A SELECT SQL query to execute.",
                }
            },
            "required": ["sql"],
        },
    },
]


async def do_list_tables() -> str:
    if not pool:
        return "Database not configured."
    async with pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT table_name, column_name, data_type
            FROM information_schema.columns
            WHERE table_schema = 'public'
            ORDER BY table_name, ordinal_position
            """
        )
    tables: dict = {}
    for row in rows:
        t = row["table_name"]
        if t not in tables:
            tables[t] = []
        tables[t].append(f"{row['column_name']} ({row['data_type']})")
    if not tables:
        return "No tables found in public schema."
    lines = []
    for table, cols in tables.items():
        lines.append(f"Table: {table}")
        lines.extend(f"  - {c}" for c in cols)
    return "\n".join(lines)


async def do_query(sql: str) -> str:
    if not pool:
        return "Database not configured."
    if _FORBIDDEN.search(sql):
        return "Error: Only SELECT queries are allowed."
    async with pool.acquire() as conn:
        rows = await conn.fetch(sql)
    if not rows:
        return "Query returned 0 rows."
    data = [dict(r) for r in rows[:200]]
    return json.dumps(data, default=str, ensure_ascii=False, indent=2)


@app.get("/health")
async def health():
    return {"ok": True}


@app.post("/mcp")
async def mcp_endpoint(request: Request):
    body = await request.json()
    method = body.get("method")
    req_id = body.get("id")

    if method == "tools/list":
        return JSONResponse({"jsonrpc": "2.0", "id": req_id, "result": {"tools": MCP_TOOLS}})

    if method == "tools/call":
        tool_name = body.get("params", {}).get("name")
        args = body.get("params", {}).get("arguments", {})
        try:
            if tool_name == "list_tables":
                result = await do_list_tables()
            elif tool_name == "query_db":
                result = await do_query(args["sql"])
            else:
                return JSONResponse({
                    "jsonrpc": "2.0",
                    "error": {"code": -32601, "message": f"Unknown tool: {tool_name}"},
                    "id": req_id,
                })
            return JSONResponse({
                "jsonrpc": "2.0",
                "result": {"content": [{"type": "text", "text": result}]},
                "id": req_id,
            })
        except Exception as e:
            return JSONResponse({
                "jsonrpc": "2.0",
                "error": {"code": -32000, "message": str(e)},
                "id": req_id,
            })

    return JSONResponse({
        "jsonrpc": "2.0",
        "error": {"code": -32601, "message": f"Unknown method: {method}"},
        "id": req_id,
    })
