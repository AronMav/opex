FROM opex-mcp-bridge:latest
RUN apt-get update && apt-get install -y --no-install-recommends git && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir mcp-server-git
ENV MCP_COMMAND='["mcp-server-git"]'
