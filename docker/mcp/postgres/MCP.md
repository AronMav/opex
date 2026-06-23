# PostgreSQL MCP

Read-only access to the OPEX database.

## Tools

- **list_tables** — shows all tables in the `public` schema with columns and types
- **query_db** — runs a SELECT query, returns up to 200 rows as JSON

## Notes

- Only SELECT is allowed; DDL/DML raises an error
- Results are capped at 200 rows to keep responses manageable
- Useful for analytics, cross-table lookups, and memory statistics
