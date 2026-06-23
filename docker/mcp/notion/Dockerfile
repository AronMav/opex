FROM --platform=linux/amd64 mcp/notion:latest AS mcp
FROM opex-mcp-bridge:latest
COPY --from=mcp /usr/local/lib/node_modules /mcp_modules
ENV MCP_COMMAND='["node", "/mcp_modules/@notionhq/notion-mcp-server/bin/cli.mjs"]'
