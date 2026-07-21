/**
 * Obsidian-compatible MCP server for note operations.
 *
 * Operates on markdown files in the vault directory (mounted read-write
 * from host workspace). Environment:
 *   VAULT_PATH - path to the vault directory (default: /workspace/storage)
 */

const fs = require("fs").promises;
const path = require("path");
const Fastify = require("fastify");
const ops = require("./ops");

const VAULT_PATH = process.env.VAULT_PATH || "/workspace/storage";
const PORT = parseInt(process.env.PORT || "8000", 10);

const app = Fastify({ logger: true });

const MCP_TOOLS = [
  {
    name: "list_notes",
    description:
      "List all notes in the vault. Returns filenames and first line (title).",
    inputSchema: {
      type: "object",
      properties: {
        limit: {
          type: "integer",
          default: 50,
          description: "Max notes to return",
        },
        query: {
          type: "string",
          description: "Filter by filename or content substring",
        },
      },
    },
  },
  {
    name: "read_note",
    description: "Read the full content of a note.",
    inputSchema: {
      type: "object",
      properties: {
        filename: {
          type: "string",
          description: "Note filename (e.g. 'my-note.md')",
        },
      },
      required: ["filename"],
    },
  },
  {
    name: "create_note",
    description:
      "Create a new note. Use timestamped or descriptive naming (YYYYMMDDHHMMSS-topic.md).",
    inputSchema: {
      type: "object",
      properties: {
        filename: {
          type: "string",
          description: "Filename for the note (e.g. '20260228-topic.md')",
        },
        content: { type: "string", description: "Markdown content of the note" },
      },
      required: ["filename", "content"],
    },
  },
  {
    name: "random_note",
    description:
      "Get a random note from the vault for learning/review.",
    inputSchema: {
      type: "object",
      properties: {},
    },
  },
  {
    name: "search_notes",
    description:
      "Search notes by content. Returns matching filenames and snippets.",
    inputSchema: {
      type: "object",
      properties: {
        query: { type: "string", description: "Search query" },
        limit: {
          type: "integer",
          default: 10,
          description: "Max results",
        },
      },
      required: ["query"],
    },
  },
  {
    name: "save_media",
    description: "Save an image into a vault folder (base64). Defaults to _System/media if folder is omitted.",
    inputSchema: {
      type: "object",
      properties: {
        filename: { type: "string" },
        content_b64: { type: "string" },
        folder: { type: "string", description: "Target subfolder, e.g. 'Видео/slug/images'. Defaults to _System/media." },
      },
      required: ["filename", "content_b64"],
    },
  },
  {
    name: "create_note",
    description: "Create a note, optionally in a subfolder.",
    inputSchema: {
      type: "object",
      properties: {
        folder: { type: "string", description: "Subfolder, e.g. 'Видео/название'" },
        filename: { type: "string" },
        content: { type: "string" },
      },
      required: ["filename", "content"],
    },
  },
  {
    name: "note_exists",
    description: "Check if a note exists in a subfolder.",
    inputSchema: {
      type: "object",
      properties: {
        folder: { type: "string" },
        filename: { type: "string" },
      },
      required: ["filename"],
    },
  },
  {
    name: "commit_vault",
    description: "git add+commit the vault.",
    inputSchema: {
      type: "object",
      properties: { message: { type: "string" } },
      required: ["message"],
    },
  },
];

async function listNotes(limit = 50, query = "") {
  let files;
  try {
    files = await fs.readdir(VAULT_PATH);
  } catch {
    return "Каталог хранилища не найден: " + VAULT_PATH;
  }

  const mdFiles = files
    .filter((f) => f.endsWith(".md") && !f.startsWith("."))
    .sort();

  const results = [];
  for (const file of mdFiles) {
    if (results.length >= limit) break;
    if (query) {
      const content = await fs
        .readFile(path.join(VAULT_PATH, file), "utf8")
        .catch(() => "");
      if (
        !file.toLowerCase().includes(query.toLowerCase()) &&
        !content.toLowerCase().includes(query.toLowerCase())
      )
        continue;
    }
    const firstLine = await fs
      .readFile(path.join(VAULT_PATH, file), "utf8")
      .then((c) => c.split("\n")[0].replace(/^#\s*/, "").trim())
      .catch(() => "");
    results.push(`${file}: ${firstLine}`);
  }

  return results.length
    ? `Найдено ${results.length} заметок:\n` + results.join("\n")
    : "Заметки не найдены.";
}

async function readNote(filename) {
  const safe = path.basename(filename);
  try {
    return await fs.readFile(path.join(VAULT_PATH, safe), "utf8");
  } catch {
    return `Заметка '${safe}' не найдена.`;
  }
}

async function randomNote() {
  let files;
  try {
    files = await fs.readdir(VAULT_PATH);
  } catch {
    return "Каталог хранилища не найден.";
  }
  const mdFiles = files.filter((f) => f.endsWith(".md") && !f.startsWith("."));
  if (!mdFiles.length) return "Нет заметок в хранилище.";
  const file = mdFiles[Math.floor(Math.random() * mdFiles.length)];
  const content = await fs
    .readFile(path.join(VAULT_PATH, file), "utf8")
    .catch(() => "");
  return `Случайная заметка: ${file}\n\n${content}`;
}

async function searchNotes(query, limit = 10) {
  let files;
  try {
    files = await fs.readdir(VAULT_PATH);
  } catch {
    return "Каталог хранилища не найден.";
  }
  const mdFiles = files.filter((f) => f.endsWith(".md") && !f.startsWith("."));
  const q = query.toLowerCase();
  const results = [];

  for (const file of mdFiles) {
    if (results.length >= limit) break;
    const content = await fs
      .readFile(path.join(VAULT_PATH, file), "utf8")
      .catch(() => "");
    const idx = content.toLowerCase().indexOf(q);
    if (idx >= 0) {
      const start = Math.max(0, idx - 50);
      const snippet = content.substring(start, idx + query.length + 50).trim();
      results.push(`${file}: ...${snippet}...`);
    }
  }

  return results.length
    ? `Найдено ${results.length}:\n` + results.join("\n\n")
    : `По запросу '${query}' ничего не найдено.`;
}

// Health check
app.get("/health", async () => ({ status: "ok" }));

// MCP endpoint
app.post("/mcp", async (request, reply) => {
  const { method, id: reqId = 1, params = {} } = request.body;

  if (method === "tools/list") {
    return { jsonrpc: "2.0", result: { tools: MCP_TOOLS }, id: reqId };
  }

  if (method === "tools/call") {
    const { name: toolName, arguments: args = {} } = params;
    let result;

    try {
      switch (toolName) {
        case "list_notes":
          result = await listNotes(args.limit, args.query);
          break;
        case "read_note":
          result = await readNote(args.filename);
          break;
        case "create_note":
          result = await ops.createNote(args.folder, args.filename, args.content);
          break;
        case "random_note":
          result = await randomNote();
          break;
        case "search_notes":
          result = await searchNotes(args.query, args.limit);
          break;
        case "save_media":
          result = await ops.saveMedia(args.filename, args.content_b64, args.folder); break;
        case "note_exists":
          result = String(await ops.noteExists(args.folder, args.filename)); break;
        case "commit_vault":
          result = await ops.commitVault(args.message); break;
        default:
          return {
            jsonrpc: "2.0",
            error: { code: -32601, message: `Unknown tool: ${toolName}` },
            id: reqId,
          };
      }

      return {
        jsonrpc: "2.0",
        result: { content: [{ type: "text", text: result }] },
        id: reqId,
      };
    } catch (e) {
      return {
        jsonrpc: "2.0",
        error: { code: -32000, message: String(e) },
        id: reqId,
      };
    }
  }

  return {
    jsonrpc: "2.0",
    error: { code: -32601, message: `Unknown method: ${method}` },
    id: reqId,
  };
});

app.listen({ port: PORT, host: "0.0.0.0" }).then(() => {
  console.log(`Obsidian MCP listening on port ${PORT}`);
});