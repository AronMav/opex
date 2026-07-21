# Obsidian MCP

Operations with notes in the vault (Obsidian-compatible format).

## Configuration

Set the `VAULT_PATH` environment variable to the vault directory.
Default: `/workspace/storage`.

## Tools

### list_notes

List notes. Optionally filter by name/content.

### read_note

- **filename** (required): Note filename

### create_note

Create a note, optionally in a subfolder.

- **filename** (required): Filename (format: YYYYMMDD-topic.md)
- **content** (required): Markdown content
- **folder** (optional): Subfolder path, e.g. `Видео/название`. Must not contain `..`.

### random_note

Random note for learning and finding connections.

### search_notes

- **query** (required): Search query against note contents

### save_media

Save an image file into `_System/media/` (base64-encoded). Rejects path traversal and non-image extensions.

- **filename** (required): Bare filename, e.g. `frame-01.jpg`. Allowed extensions: `.jpg`, `.jpeg`, `.png`, `.webp`. Must not contain path separators.
- **content_b64** (required): Base64-encoded image bytes. Max 10 MB.

### note_exists

Check whether a note already exists at a given path. Returns `"true"` or `"false"`.

- **filename** (required): Note filename (`.md` appended if missing)
- **folder** (optional): Subfolder path, e.g. `Видео/тест`

### commit_vault

Run `git add -A && git commit` on the vault directory. Useful after writing notes or media to persist a snapshot.

- **message** (required): Commit message (passed as a direct argument — no shell interpolation)