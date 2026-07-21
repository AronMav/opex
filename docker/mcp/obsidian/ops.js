const fs = require("fs").promises;
const path = require("path");
const { execFile } = require("child_process");

const VAULT_PATH = () => process.env.VAULT_PATH || "/workspace/storage";
const MEDIA_EXT = new Set([".jpg", ".jpeg", ".png", ".webp"]);
const MAX_MEDIA_BYTES = 10 * 1024 * 1024;

function safeFolder(folder) {
  const norm = path.posix.normalize((folder || "").replace(/\\/g, "/"));
  if (norm.startsWith("..") || norm.includes("../") || path.isAbsolute(norm)) {
    throw new Error(`invalid folder: ${folder}`);
  }
  return norm.replace(/^\/+/, "");
}

async function saveMedia(filename, contentB64, folder) {
  const base = path.basename(filename);
  if (base !== filename) throw new Error(`invalid media name: ${filename}`);
  if (!MEDIA_EXT.has(path.extname(base).toLowerCase())) throw new Error(`extension not allowed: ${base}`);
  const buf = Buffer.from(contentB64, "base64");
  if (buf.length > MAX_MEDIA_BYTES) throw new Error(`media too large: ${buf.length}`);
  const rel = safeFolder(folder || "_System/media");
  const dir = path.join(VAULT_PATH(), rel);
  await fs.mkdir(dir, { recursive: true });
  await fs.writeFile(path.join(dir, base), buf);
  return `Сохранено: ${rel}/${base}`;
}

async function createNote(folder, filename, content) {
  const sf = safeFolder(folder);
  let name = path.basename(filename);
  if (!name.endsWith(".md")) name += ".md";
  const dir = path.join(VAULT_PATH(), sf);
  await fs.mkdir(dir, { recursive: true });
  const file = path.join(dir, name);
  try { await fs.access(file); throw new Error(`note already exists: ${sf}/${name}`); } catch (e) { if (e.code !== "ENOENT") throw e; }
  await fs.writeFile(file, content, "utf8");
  return `Создана заметка: ${sf}/${name}`;
}

async function noteExists(folder, filename) {
  const sf = safeFolder(folder);
  let name = path.basename(filename);
  if (!name.endsWith(".md")) name += ".md";
  try { await fs.access(path.join(VAULT_PATH(), sf, name)); return true; } catch { return false; }
}

function commitVault(message) {
  return new Promise((resolve) => {
    execFile("git", ["-C", VAULT_PATH(), "add", "-A"], () => {
      execFile("git", ["-C", VAULT_PATH(), "-c", "user.name=opex", "-c", "user.email=opex@local",
        "commit", "-m", String(message)], (err, stdout, stderr) => {
        const text = (stdout || "") + (stderr || "");
        if (err && !/nothing to commit/i.test(text)) resolve(`commit error: ${text.slice(0, 200)}`);
        else resolve(/nothing to commit/i.test(text) ? "nothing to commit" : "committed");
      });
    });
  });
}

module.exports = { saveMedia, createNote, noteExists, commitVault, safeFolder };