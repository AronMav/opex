// Minimal test runner for the Obsidian MCP file ops. No framework — plain asserts.
const fs = require("fs");
const os = require("os");
const path = require("path");
const assert = require("assert");

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "zk-test-"));
process.env.ZETTELKASTEN_PATH = tmp;
fs.mkdirSync(path.join(tmp, "_System", "media"), { recursive: true });

const ops = require("./ops"); // pure functions extracted from app.js

(async () => {
  // save_media
  const b64 = Buffer.from([0xff, 0xd8, 0xff, 0x00]).toString("base64");
  await ops.saveMedia("frame-01.jpg", b64);
  assert.ok(fs.existsSync(path.join(tmp, "_System/media/frame-01.jpg")), "media saved");
  await assert.rejects(ops.saveMedia("../evil.jpg", b64), /name|path|invalid/i);
  await assert.rejects(ops.saveMedia("x.exe", b64), /extension|allowed/i);

  // create_note in a subfolder
  await ops.createNote("Видео/тест", "конспект.md", "# Заметка\n");
  assert.ok(fs.existsSync(path.join(tmp, "Видео/тест/конспект.md")), "note in subfolder");
  await assert.rejects(ops.createNote("../escape", "x.md", "y"), /folder|invalid|\.\./i);
  // collision must throw, not silently succeed
  await assert.rejects(ops.createNote("Видео/тест", "конспект.md", "дубль"), /already exists/i, "duplicate note must reject");

  // note_exists
  assert.equal(await ops.noteExists("Видео/тест", "конспект.md"), true);
  assert.equal(await ops.noteExists("Видео/нет", "конспект.md"), false);

  // commit_vault — init a repo first
  require("child_process").execSync("git init && git add -A && git commit -m init", { cwd: tmp });
  await ops.createNote("Видео/тест2", "конспект.md", "# Два\n");
  const out = await ops.commitVault("видео-конспект: тест");
  assert.ok(/тест|commit|nothing/i.test(out), "commit ran");

  console.log("ALL MCP OPS TESTS PASSED");
})().catch((e) => { console.error("FAIL:", e); process.exit(1); });
