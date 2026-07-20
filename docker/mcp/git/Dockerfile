FROM opex-mcp-bridge:latest
RUN apt-get update && apt-get install -y --no-install-recommends git && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir mcp-server-git
# Mount the deploy source tree at /src in docker-compose.yml; default the git
# server to that repo so calls without an explicit repo_path still work.
ENV MCP_COMMAND='["mcp-server-git", "--repository", "/src"]'
