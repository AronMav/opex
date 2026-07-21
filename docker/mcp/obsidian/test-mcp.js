// Minimal test runner for the Obsidian MCP file ops. No framework — plain asserts.
const fs = require("fs");
const os = require("os");
const path = require("path");
const assert = require("assert");

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "vault-test-"));
process.env.VAULT_PATH = tmp;
fs.mkdirSync(path.join(tmp, "_System", "media"), { recursive: true });

const ops = require("./ops"); // pure functions extracted from app.js

(async () => {
  // save_media — default folder (_System/media)
  const b64 = Buffer.from([0xff, 0xd8, 0xff, 0x00]).toString("base64");
  await ops.saveMedia("frame-01.jpg", b64);
  assert.ok(fs.existsSync(path.join(tmp, "_System/media/frame-01.jpg")), "media saved to default folder");
  await assert.rejects(ops.saveMedia("../evil.jpg", b64), /name|path|invalid/i);
  await assert.rejects(ops.saveMedia("x.exe", b64), /extension|allowed/i);

  // save_media — custom folder (Видео/test/images)
  await ops.saveMedia("t.jpg", b64, "Видео/test/images");
  assert.ok(fs.existsSync(path.join(tmp, "Видео/test/images/t.jpg")), "media saved to custom folder");
  // cleanup custom folder test file
  fs.rmSync(path.join(tmp, "Видео/test/images/t.jpg"));

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