FROM node:22-slim
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends git && rm -rf /var/lib/apt/lists/*
COPY package.json .
RUN npm install --production
COPY app.js ops.js MCP.md ./
EXPOSE 8000
CMD ["node", "app.js"]
