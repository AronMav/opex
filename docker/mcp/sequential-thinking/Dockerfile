FROM --platform=linux/amd64 mcp/sequentialthinking:latest AS mcp
FROM opex-mcp-bridge:latest
COPY --from=mcp /app /mcp_server
ENV MCP_COMMAND='["node", "/mcp_server/dist/index.js"]'
