# Workspace Obsidian Viewer + File Manager — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Превратить вкладку `/workspace/` в полноценный Obsidian-просмотрщик + файловый менеджер: рендер картинок/PDF, инлайн-картинки и Obsidian-конструкции в `.md`, рекурсивное удаление папок, создание папок, переименование, загрузка и скачивание файлов.

**Architecture:** Бэкенд (`gateway/handlers/workspace.rs`) определяет бинарность файла и отдаёт подписанный `/workspace-files/`-URL вместо падения на `read_to_string`; новые POST-роуты (`/sign`, `/mkdir`, `/rename`, `/upload`) с путём в теле/форме (не в URL-сегменте — чтобы не конфликтовать с catch-all `{*path}`). Фронтенд выбирает просмотрщик по `is_binary`+`mime`, добавляет файловые операции в дерево, и заменяет TipTap-редактор `.md` на CodeMirror 6 Live Preview с декорациями (markdown-исходник — источник истины, сохранение без потерь).

**Tech Stack:** Rust + axum 0.8 + tokio; React 19 + Next.js + CodeMirror 6 (`@uiw/react-codemirror`, `@codemirror/view`, `@codemirror/lang-markdown`, `@codemirror/language`); vitest.

**Спецификация:** [docs/architecture/2026-06-27-workspace-obsidian-viewer-design.md](2026-06-27-workspace-obsidian-viewer-design.md)

## Global Constraints

- **Только rustls-tls, никакого OpenSSL.** Не добавлять зависимости, тянущие OpenSSL.
- **Новых тяжёлых зависимостей не вводить.** Frontend: только уже установленные CM6-пакеты (`@codemirror/language` добавить явно — он уже есть транзитивно). PDF — нативный `<iframe>`, без pdf.js.
- **Все пути — через `resolve_workspace_path`/`resolve_within`** (canonicalize + `starts_with(base)`). Ни один новый роут не пишет/читает/удаляет вне `workspace/`.
- **Подписанные URL — только существующий HMAC** (`uploads::mint_workspace_file_url` / `verify_workspace_file_url`). Не изобретать свою подпись.
- **Инвариант редактора `.md`:** markdown-исходник — единственный источник истины; `onChange`/`onSave` отдают ровно текущий текст буфера. Декорации не меняют сохраняемый текст.
- **Upload:** только basename (`Path::file_name`), лимит **50 MB**/файл, `DefaultBodyLimit` на под-роутере (дефолт axum 2 MiB).
- **TDD:** сначала падающий тест, потом минимальная реализация. Частые коммиты (один на задачу).
- **Коммиты:** БЕЗ строки `Co-Authored-By` (правило пользователя). Работаем в текущей ветке (master).
- **Деплой:** Rust — `make remote-deploy`; UI — отдельной сборкой (не авто-синкается).

**Команды проверки:**
- Rust один тест: `cargo test -p opex-core <test_name> -- --nocapture`
- Rust весь крейт: `cargo test -p opex-core`
- Rust clippy: `cargo clippy -p opex-core --all-targets -- -D warnings`
- Frontend один файл: `cd ui && npx vitest run <path>`
- Frontend всё: `cd ui && npm test`

---

# Фаза 1 — Backend: API файлов и отдача бинаря

Все изменения — в `crates/opex-core/src/gateway/handlers/workspace.rs`. `routes()` уже мёржится в `gateway/mod.rs:128` — менять `mod.rs` не нужно (роуты добавляются внутри `routes()`).

## Task 1: Классификация бинарности + base-параметризованный resolve

Выносим логику резолва пути в base-параметризованную функцию (тестируемо с tempdir) и добавляем чистый классификатор текст/бинарь.

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/workspace.rs`
- Test: тот же файл (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `fn is_binary_filename(name: &str) -> bool`
  - `async fn resolve_within(base: &Path, rel: &str) -> Result<(PathBuf, PathBuf), (StatusCode, Json<Value>)>` — возвращает `(base_canonical, target_canonical)`.
  - `resolve_workspace_path(rel)` становится тонкой обёрткой над `resolve_within(Path::new(WORKSPACE_DIR), rel)`.

- [ ] **Step 1: Написать падающие тесты**

В конец `workspace.rs` добавить:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_classification_by_extension() {
        for n in ["a.png", "a.JPG", "photo.jpeg", "x.webp", "y.gif", "doc.pdf", "icon.svg"] {
            assert!(is_binary_filename(n), "{n} must be binary");
        }
        for n in ["note.md", "data.json", "cfg.toml", "log.txt", "s.yaml", "x.csv"] {
            assert!(!is_binary_filename(n), "{n} must be text");
        }
    }

    #[test]
    fn unknown_extension_defaults_to_text() {
        // No extension / unknown → treated as text (browse falls back to UTF-8 probe).
        assert!(!is_binary_filename("Makefile"));
        assert!(!is_binary_filename("weird.xyz"));
    }

    #[tokio::test]
    async fn resolve_within_rejects_traversal() {
        let base = tempfile::tempdir().unwrap();
        let res = resolve_within(base.path(), "../escape.txt").await;
        assert!(res.is_err(), "traversal must be denied");
    }

    #[tokio::test]
    async fn resolve_within_accepts_inside() {
        let base = tempfile::tempdir().unwrap();
        tokio::fs::write(base.path().join("ok.md"), b"hi").await.unwrap();
        let (b, t) = resolve_within(base.path(), "ok.md").await.unwrap();
        assert!(t.starts_with(&b));
    }
}
```

- [ ] **Step 2: Запустить — убедиться, что не компилируется/падает**

Run: `cargo test -p opex-core binary_classification_by_extension -- --nocapture`
Expected: FAIL — `is_binary_filename`/`resolve_within` не существуют.

- [ ] **Step 3: Реализовать**

Добавить в `workspace.rs` (рядом с `resolve_workspace_path`):

```rust
/// Extensions treated as binary/media — browse returns a signed URL, never UTF-8.
const BINARY_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "ico", "svg", "pdf",
    "mp3", "wav", "ogg", "opus", "m4a", "mp4", "webm", "mov",
    "zip", "gz", "tar", "bin", "wasm",
];

pub(crate) fn is_binary_filename(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .map(|e| BINARY_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}
```

Затем рефакторить резолв: переименовать тело `resolve_workspace_path` в `resolve_within(base, rel)` (заменив `let base = std::path::Path::new(crate::config::WORKSPACE_DIR);` на параметр `base: &Path`), и оставить обёртку:

```rust
async fn resolve_within(
    base: &std::path::Path,
    rel_path: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), (StatusCode, Json<Value>)> {
    let _ = tokio::fs::create_dir_all(base).await;
    let target = base.join(rel_path);

    let base_canonical = tokio::fs::canonicalize(base).await
        .unwrap_or_else(|_| base.to_path_buf());

    let target_canonical = if target.exists() {
        tokio::fs::canonicalize(&target).await.unwrap_or_else(|_| target.clone())
    } else if let Some(parent) = target.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
        let parent_canonical = tokio::fs::canonicalize(parent).await
            .unwrap_or_else(|_| parent.to_path_buf());
        let file_name = target.file_name().unwrap_or_default();
        parent_canonical.join(file_name)
    } else {
        target.clone()
    };

    if !target_canonical.starts_with(&base_canonical) {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error": "path traversal denied"}))));
    }
    Ok((base_canonical, target_canonical))
}

async fn resolve_workspace_path(
    rel_path: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), (StatusCode, Json<Value>)> {
    resolve_within(std::path::Path::new(crate::config::WORKSPACE_DIR), rel_path).await
}
```

Add `use std::path::Path;` if not present (or use fully-qualified as above).

- [ ] **Step 4: Запустить — убедиться, что проходит**

Run: `cargo test -p opex-core workspace -- --nocapture`
Expected: PASS (4 новых теста + существующие компилируются).

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/workspace.rs
git commit -m "feat(workspace): binary classification + base-parameterized path resolve"
```

---

## Task 2: Binary-aware browse — подписанный URL вместо UTF-8-падения

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/workspace.rs`
- Test: тот же файл

**Interfaces:**
- Consumes: `is_binary_filename`, `resolve_within` (Task 1), `uploads::{mint_workspace_file_url, guess_mime_from_extension}`.
- Produces:
  - `async fn build_file_response(base: &Path, rel: &str, key: &[u8; 32], ttl: u64) -> Result<Value, (StatusCode, Json<Value>)>`
  - `api_workspace_browse` теперь берёт `State<InfraServices>` + `State<ConfigServices>`.
  - Бинарный ответ: `{ "is_binary": true, "mime": String, "size": u64, "url": String, "path": String }`.

- [ ] **Step 1: Написать падающий тест**

Добавить в `mod tests`:

```rust
#[tokio::test]
async fn build_response_text_returns_content() {
    let base = tempfile::tempdir().unwrap();
    tokio::fs::write(base.path().join("n.md"), b"# Hi").await.unwrap();
    let v = build_file_response(base.path(), "n.md", &[7u8; 32], 3600).await.unwrap();
    assert_eq!(v["content"], "# Hi");
    assert_eq!(v["is_dir"], false);
    assert!(v.get("is_binary").is_none());
}

#[tokio::test]
async fn build_response_binary_returns_signed_url() {
    let base = tempfile::tempdir().unwrap();
    // 1-byte PNG-ish binary (invalid UTF-8 byte 0xFF ensures non-text).
    tokio::fs::write(base.path().join("img.png"), [0xFFu8, 0x00, 0x01]).await.unwrap();
    let v = build_file_response(base.path(), "img.png", &[7u8; 32], 3600).await.unwrap();
    assert_eq!(v["is_binary"], true);
    assert_eq!(v["mime"], "image/png");
    assert_eq!(v["size"], 3);
    let url = v["url"].as_str().unwrap();
    assert!(url.starts_with("/workspace-files/img.png?sig="), "got {url}");
}
```

- [ ] **Step 2: Запустить — убедиться, что падает**

Run: `cargo test -p opex-core build_response -- --nocapture`
Expected: FAIL — `build_file_response` не существует.

- [ ] **Step 3: Реализовать**

Добавить функцию + изменить хендлер. Сначала импорты в начало файла:

```rust
use crate::gateway::clusters::{ConfigServices, InfraServices};
use axum::extract::State;
```

Функция:

```rust
async fn build_file_response(
    base: &std::path::Path,
    rel: &str,
    key: &[u8; 32],
    ttl: u64,
) -> Result<Value, (StatusCode, Json<Value>)> {
    let (base_canon, target) = resolve_within(base, rel).await?;

    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let bytes = tokio::fs::read(&target).await.map_err(|e| {
        let status = if e.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, Json(json!({"error": e.to_string()})))
    })?;

    // Binary if extension says so OR content is not valid UTF-8.
    let is_binary = is_binary_filename(name) || std::str::from_utf8(&bytes).is_err();

    if is_binary {
        // Re-derive workspace-relative path so the signed URL matches what
        // serve_workspace_file canonicalizes (C-2 bug class).
        let rel_for_url = target
            .strip_prefix(&base_canon)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| rel.to_string());
        let url = crate::uploads::mint_workspace_file_url(&rel_for_url, key, ttl);
        let mime = crate::uploads::guess_mime_from_extension(name);
        Ok(json!({
            "is_binary": true,
            "mime": mime,
            "size": bytes.len(),
            "url": url,
            "path": rel,
            "is_dir": false,
        }))
    } else {
        let content = String::from_utf8(bytes).unwrap_or_default();
        Ok(json!({ "content": content, "path": rel, "is_dir": false }))
    }
}
```

Изменить `api_workspace_browse`: добавить State и заменить файловую ветку:

```rust
pub(crate) async fn api_workspace_browse(
    State(infra): State<InfraServices>,
    State(cfg): State<ConfigServices>,
    path: Option<axum::extract::Path<String>>,
) -> impl IntoResponse {
    let rel_path = path.as_ref().map_or(".", |p| p.as_str());

    let (_, target) = match resolve_workspace_path(if rel_path.is_empty() { "." } else { rel_path }).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };

    if target.is_dir() {
        match list_dir_entries(&target).await {
            Ok(entries) => Json(json!({ "files": entries, "is_dir": true })).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
        }
    } else if target.is_file() {
        let key = infra.secrets.get_upload_hmac_key();
        let ttl = cfg.config.uploads.signed_url_ttl_secs;
        match build_file_response(std::path::Path::new(crate::config::WORKSPACE_DIR), rel_path, &key, ttl).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => e.into_response(),
        }
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response()
    }
}
```

- [ ] **Step 4: Запустить — убедиться, что проходит**

Run: `cargo test -p opex-core workspace -- --nocapture`
Expected: PASS.

Run: `cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: чисто.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/workspace.rs
git commit -m "feat(workspace): browse returns signed URL for binary files instead of UTF-8 failure"
```

---

## Task 3: `POST /api/workspace/sign` — батч-подпись инлайн-ассетов

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/workspace.rs`
- Test: тот же файл

**Interfaces:**
- Produces:
  - `async fn build_sign_map(base: &Path, paths: &[String], key: &[u8;32], ttl: u64) -> serde_json::Map<String, Value>` — внешние/несуществующие пути пропускаются.
  - Роут `POST /api/workspace/sign`, тело `{ "paths": [String] }` → `{ "url_by_path": { path: url } }`.

- [ ] **Step 1: Написать падающий тест**

```rust
#[tokio::test]
async fn sign_map_skips_external_and_missing() {
    let base = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(base.path().join("note/images")).await.unwrap();
    tokio::fs::write(base.path().join("note/images/x.png"), b"x").await.unwrap();

    let paths = vec![
        "note/images/x.png".to_string(),
        "note/images/missing.png".to_string(),
        "../../etc/passwd".to_string(),
    ];
    let m = build_sign_map(base.path(), &paths, &[3u8; 32], 3600).await;

    assert!(m.contains_key("note/images/x.png"));
    assert!(!m.contains_key("note/images/missing.png"), "missing skipped");
    assert!(!m.contains_key("../../etc/passwd"), "external skipped");
    let url = m["note/images/x.png"].as_str().unwrap();
    assert!(url.starts_with("/workspace-files/note/images/x.png?sig="), "got {url}");
}
```

- [ ] **Step 2: Запустить — падает**

Run: `cargo test -p opex-core sign_map -- --nocapture`
Expected: FAIL — `build_sign_map` не существует.

- [ ] **Step 3: Реализовать**

```rust
#[derive(Debug, Deserialize)]
pub(crate) struct SignRequest {
    paths: Vec<String>,
}

async fn build_sign_map(
    base: &std::path::Path,
    paths: &[String],
    key: &[u8; 32],
    ttl: u64,
) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for p in paths {
        let Ok((base_canon, target)) = resolve_within(base, p).await else { continue };
        if !target.is_file() { continue }
        let rel_for_url = target
            .strip_prefix(&base_canon)
            .map(|x| x.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| p.clone());
        let url = crate::uploads::mint_workspace_file_url(&rel_for_url, key, ttl);
        out.insert(p.clone(), Value::String(url));
    }
    out
}

pub(crate) async fn api_workspace_sign(
    State(infra): State<InfraServices>,
    State(cfg): State<ConfigServices>,
    Json(req): Json<SignRequest>,
) -> impl IntoResponse {
    let key = infra.secrets.get_upload_hmac_key();
    let ttl = cfg.config.uploads.signed_url_ttl_secs;
    let map = build_sign_map(std::path::Path::new(crate::config::WORKSPACE_DIR), &req.paths, &key, ttl).await;
    Json(json!({ "url_by_path": Value::Object(map) }))
}
```

Зарегистрировать роут в `routes()`:

```rust
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/workspace", get(api_workspace_browse))
        .route("/api/workspace/sign", post(api_workspace_sign))
        .route("/api/workspace/{*path}", get(api_workspace_browse).put(api_workspace_write).delete(api_workspace_delete))
}
```

Добавить в импорты `routing`: `use axum::routing::{get, post};`.

- [ ] **Step 4: Запустить — проходит**

Run: `cargo test -p opex-core sign_map -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/workspace.rs
git commit -m "feat(workspace): POST /api/workspace/sign batch-signs inline asset URLs"
```

---

## Task 4: Рекурсивное удаление папки (`?recursive=true`) + гард корня

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/workspace.rs`
- Test: тот же файл

**Interfaces:**
- Produces:
  - `async fn do_delete(base: &Path, rel: &str, recursive: bool) -> Result<(), (StatusCode, Json<Value>)>`
  - `api_workspace_delete` принимает `Query<DeleteQuery>` с `recursive: Option<bool>`.

- [ ] **Step 1: Написать падающий тест**

```rust
#[tokio::test]
async fn delete_nonempty_dir_requires_recursive() {
    let base = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(base.path().join("d")).await.unwrap();
    tokio::fs::write(base.path().join("d/f.txt"), b"x").await.unwrap();

    // Without recursive → error (409).
    let err = do_delete(base.path(), "d", false).await.unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT);

    // With recursive → removed.
    do_delete(base.path(), "d", true).await.unwrap();
    assert!(!base.path().join("d").exists());
}

#[tokio::test]
async fn delete_refuses_workspace_root() {
    let base = tempfile::tempdir().unwrap();
    let err = do_delete(base.path(), ".", true).await.unwrap_err();
    assert_eq!(err.0, StatusCode::FORBIDDEN);
}
```

- [ ] **Step 2: Запустить — падает**

Run: `cargo test -p opex-core delete_ -- --nocapture`
Expected: FAIL — `do_delete` не существует.

- [ ] **Step 3: Реализовать**

```rust
#[derive(Debug, Deserialize)]
pub(crate) struct DeleteQuery {
    #[serde(default)]
    recursive: bool,
}

async fn do_delete(
    base: &std::path::Path,
    rel: &str,
    recursive: bool,
) -> Result<(), (StatusCode, Json<Value>)> {
    let (base_canon, target) = resolve_within(base, rel).await?;

    // Never delete the workspace root itself.
    if target == base_canon {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error": "cannot delete workspace root"}))));
    }

    if target.is_dir() {
        if recursive {
            tokio::fs::remove_dir_all(&target).await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))
        } else {
            match tokio::fs::remove_dir(&target).await {
                Ok(()) => Ok(()),
                Err(e) if matches!(e.raw_os_error(), Some(39 | 145)) => {
                    Err((StatusCode::CONFLICT, Json(json!({"error": "Directory is not empty"}))))
                }
                Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()})))),
            }
        }
    } else {
        tokio::fs::remove_file(&target).await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))
    }
}
```

Заменить тело `api_workspace_delete`:

```rust
pub(crate) async fn api_workspace_delete(
    axum::extract::Path(rel_path): axum::extract::Path<String>,
    axum::extract::Query(q): axum::extract::Query<DeleteQuery>,
) -> impl IntoResponse {
    match do_delete(std::path::Path::new(crate::config::WORKSPACE_DIR), &rel_path, q.recursive).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => e.into_response(),
    }
}
```

- [ ] **Step 4: Запустить — проходит**

Run: `cargo test -p opex-core delete_ -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/workspace.rs
git commit -m "feat(workspace): recursive folder delete via ?recursive=true with root guard"
```

---

## Task 5: `POST /api/workspace/mkdir` + `POST /api/workspace/rename`

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/workspace.rs`
- Test: тот же файл

**Interfaces:**
- Produces:
  - `async fn do_mkdir(base: &Path, rel: &str) -> Result<(), (StatusCode, Json<Value>)>`
  - `async fn do_rename(base: &Path, from: &str, to: &str) -> Result<(), (StatusCode, Json<Value>)>`
  - Роуты `POST /api/workspace/mkdir` (`{path}`), `POST /api/workspace/rename` (`{from,to}`).

- [ ] **Step 1: Написать падающий тест**

```rust
#[tokio::test]
async fn mkdir_creates_nested() {
    let base = tempfile::tempdir().unwrap();
    do_mkdir(base.path(), "a/b/c").await.unwrap();
    assert!(base.path().join("a/b/c").is_dir());
    // Idempotent.
    do_mkdir(base.path(), "a/b/c").await.unwrap();
}

#[tokio::test]
async fn rename_moves_file_refuses_collision() {
    let base = tempfile::tempdir().unwrap();
    tokio::fs::write(base.path().join("old.md"), b"x").await.unwrap();
    do_rename(base.path(), "old.md", "new.md").await.unwrap();
    assert!(base.path().join("new.md").exists());
    assert!(!base.path().join("old.md").exists());

    tokio::fs::write(base.path().join("a.md"), b"a").await.unwrap();
    let err = do_rename(base.path(), "a.md", "new.md").await.unwrap_err();
    assert_eq!(err.0, StatusCode::CONFLICT, "collision must 409");
}
```

- [ ] **Step 2: Запустить — падает**

Run: `cargo test -p opex-core "mkdir_creates_nested" -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

```rust
#[derive(Debug, Deserialize)]
pub(crate) struct MkdirRequest { path: String }

#[derive(Debug, Deserialize)]
pub(crate) struct RenameRequest { from: String, to: String }

async fn do_mkdir(base: &std::path::Path, rel: &str) -> Result<(), (StatusCode, Json<Value>)> {
    let (_, target) = resolve_within(base, rel).await?;
    tokio::fs::create_dir_all(&target).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))
}

async fn do_rename(base: &std::path::Path, from: &str, to: &str) -> Result<(), (StatusCode, Json<Value>)> {
    let (_, from_t) = resolve_within(base, from).await?;
    let (_, to_t) = resolve_within(base, to).await?;
    if to_t.exists() {
        return Err((StatusCode::CONFLICT, Json(json!({"error": "target already exists"}))));
    }
    if let Some(parent) = to_t.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    tokio::fs::rename(&from_t, &to_t).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))
}

pub(crate) async fn api_workspace_mkdir(Json(req): Json<MkdirRequest>) -> impl IntoResponse {
    match do_mkdir(std::path::Path::new(crate::config::WORKSPACE_DIR), &req.path).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => e.into_response(),
    }
}

pub(crate) async fn api_workspace_rename(Json(req): Json<RenameRequest>) -> impl IntoResponse {
    match do_rename(std::path::Path::new(crate::config::WORKSPACE_DIR), &req.from, &req.to).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => e.into_response(),
    }
}
```

Зарегистрировать роуты в `routes()` (рядом с `/sign`):

```rust
        .route("/api/workspace/sign", post(api_workspace_sign))
        .route("/api/workspace/mkdir", post(api_workspace_mkdir))
        .route("/api/workspace/rename", post(api_workspace_rename))
```

- [ ] **Step 4: Запустить — проходит**

Run: `cargo test -p opex-core workspace -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/workspace.rs
git commit -m "feat(workspace): mkdir + rename endpoints"
```

---

## Task 6: `POST /api/workspace/upload` (multipart + DefaultBodyLimit)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/workspace.rs`
- Test: тот же файл

**Interfaces:**
- Produces:
  - `async fn save_upload(base: &Path, dir: &str, filename: &str, bytes: &[u8]) -> Result<String, (StatusCode, Json<Value>)>` — возвращает сохранённое относительное имя; basename-санитайз + лимит 50 MB.
  - Роут `POST /api/workspace/upload` (multipart: поле `dir` + поля `file`), собирается в под-роутере с `DefaultBodyLimit`.

- [ ] **Step 1: Написать падающий тест**

```rust
#[tokio::test]
async fn upload_sanitizes_basename_and_writes() {
    let base = tempfile::tempdir().unwrap();
    // Path components in filename are stripped to basename.
    let rel = save_upload(base.path(), "sub", "../../evil.png", b"data").await.unwrap();
    assert_eq!(rel, "sub/evil.png");
    assert_eq!(tokio::fs::read(base.path().join("sub/evil.png")).await.unwrap(), b"data");
}

#[tokio::test]
async fn upload_rejects_oversize() {
    let base = tempfile::tempdir().unwrap();
    let big = vec![0u8; MAX_UPLOAD_BYTES + 1];
    let err = save_upload(base.path(), "", "big.bin", &big).await.unwrap_err();
    assert_eq!(err.0, StatusCode::PAYLOAD_TOO_LARGE);
}
```

- [ ] **Step 2: Запустить — падает**

Run: `cargo test -p opex-core upload_ -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

Импорты в начало файла:

```rust
use axum::extract::{DefaultBodyLimit, Multipart};
```

Код:

```rust
pub(crate) const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;

async fn save_upload(
    base: &std::path::Path,
    dir: &str,
    filename: &str,
    bytes: &[u8],
) -> Result<String, (StatusCode, Json<Value>)> {
    if bytes.len() > MAX_UPLOAD_BYTES {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"error": "file too large"}))));
    }
    let basename = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .ok_or((StatusCode::BAD_REQUEST, Json(json!({"error": "invalid filename"}))))?;

    let rel = if dir.is_empty() { basename.to_string() } else { format!("{}/{}", dir.trim_end_matches('/'), basename) };
    let (_, target) = resolve_within(base, &rel).await?;
    if let Some(parent) = target.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    tokio::fs::write(&target, bytes).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    Ok(rel)
}

pub(crate) async fn api_workspace_upload(mut multipart: Multipart) -> impl IntoResponse {
    let base = std::path::Path::new(crate::config::WORKSPACE_DIR);
    let mut dir = String::new();
    let mut saved: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        if name == "dir" {
            dir = field.text().await.unwrap_or_default();
        } else if name == "file" {
            let filename = field.file_name().unwrap_or("file").to_string();
            match field.bytes().await {
                Ok(bytes) => match save_upload(base, &dir, &filename, &bytes).await {
                    Ok(rel) => saved.push(rel),
                    Err((_, e)) => errors.push(format!("{}: {}", filename, e.0["error"])),
                },
                Err(e) => errors.push(format!("{filename}: {e}")),
            }
        }
    }
    Json(json!({ "ok": errors.is_empty(), "saved": saved, "errors": errors })).into_response()
}
```

> Примечание: `dir` должно прийти ДО полей `file` (фронт формирует FormData в этом порядке — см. Task 7).

Зарегистрировать upload в собственном под-роутере с лимитом тела. Изменить `routes()`:

```rust
pub(crate) fn routes() -> Router<AppState> {
    let upload = Router::new()
        .route("/api/workspace/upload", post(api_workspace_upload))
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES + 1024 * 1024));

    Router::new()
        .route("/api/workspace", get(api_workspace_browse))
        .route("/api/workspace/sign", post(api_workspace_sign))
        .route("/api/workspace/mkdir", post(api_workspace_mkdir))
        .route("/api/workspace/rename", post(api_workspace_rename))
        .route("/api/workspace/{*path}", get(api_workspace_browse).put(api_workspace_write).delete(api_workspace_delete))
        .merge(upload)
}
```

- [ ] **Step 4: Запустить — проходит**

Run: `cargo test -p opex-core workspace -- --nocapture`
Expected: PASS.

Run: `cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: чисто.

- [ ] **Step 5: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/workspace.rs
git commit -m "feat(workspace): multipart upload endpoint with 50MB body limit"
```

---

**Фаза 1 завершена.** Бэкенд готов: бинарь не падает, есть подпись, рекурсивное удаление, mkdir/rename/upload. Деплой по желанию: `make remote-deploy`.

---

# Фаза 2 — Frontend: просмотрщики и файловые операции

## Task 7: API-слой и типы

**Files:**
- Modify: `ui/src/types/api.ts` (расширить ответ файла)
- Modify: `ui/src/lib/api.ts` (новые helper-функции)
- Test: `ui/src/lib/__tests__/workspace-api.test.ts` (Create)

**Interfaces:**
- Produces (в `ui/src/lib/api.ts`):
  - `type WorkspaceFile = { content: string; path: string; is_dir: false } | { is_binary: true; mime: string; size: number; url: string; path: string; is_dir: false }`
  - `signWorkspacePaths(paths: string[]): Promise<Record<string,string>>`
  - `wsMkdir(path: string)`, `wsRename(from: string, to: string)`, `wsDeleteRecursive(path: string)`, `wsUpload(dir: string, files: File[])`
  - `function isBinaryFile(r: WorkspaceFile): r is Extract<WorkspaceFile, {is_binary: true}>`

- [ ] **Step 1: Написать падающий тест**

`ui/src/lib/__tests__/workspace-api.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { isBinaryFile } from "@/lib/api";

describe("isBinaryFile", () => {
  it("narrows binary responses", () => {
    expect(isBinaryFile({ is_binary: true, mime: "image/png", size: 1, url: "/x", path: "x.png", is_dir: false })).toBe(true);
    expect(isBinaryFile({ content: "hi", path: "n.md", is_dir: false })).toBe(false);
  });
});
```

- [ ] **Step 2: Запустить — падает**

Run: `cd ui && npx vitest run src/lib/__tests__/workspace-api.test.ts`
Expected: FAIL — `isBinaryFile` не экспортируется.

- [ ] **Step 3: Реализовать**

В `ui/src/types/api.ts` рядом с `FileEntry` добавить:

```ts
export type WorkspaceFile =
  | { content: string; path: string; is_dir: false }
  | { is_binary: true; mime: string; size: number; url: string; path: string; is_dir: false };
```

В `ui/src/lib/api.ts` (в конец) добавить:

```ts
import type { WorkspaceFile } from "@/types/api";

export function isBinaryFile(
  r: WorkspaceFile,
): r is Extract<WorkspaceFile, { is_binary: true }> {
  return "is_binary" in r && r.is_binary === true;
}

export const signWorkspacePaths = (paths: string[]) =>
  apiPost<{ url_by_path: Record<string, string> }>("/api/workspace/sign", { paths }).then((r) => r.url_by_path);

export const wsMkdir = (path: string) => apiPost("/api/workspace/mkdir", { path });
export const wsRename = (from: string, to: string) => apiPost("/api/workspace/rename", { from, to });
export const wsDeleteRecursive = (path: string) =>
  apiDelete(`/api/workspace/${path}?recursive=true`);

export function wsUpload(dir: string, files: File[]) {
  const fd = new FormData();
  fd.append("dir", dir); // MUST be appended before files (backend reads dir first)
  for (const f of files) fd.append("file", f);
  return apiPostFormData<{ ok: boolean; saved: string[]; errors: string[] }>("/api/workspace/upload", fd);
}
```

- [ ] **Step 4: Запустить — проходит**

Run: `cd ui && npx vitest run src/lib/__tests__/workspace-api.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ui/src/types/api.ts ui/src/lib/api.ts ui/src/lib/__tests__/workspace-api.test.ts
git commit -m "feat(workspace-ui): API helpers for sign/mkdir/rename/upload/recursive-delete"
```

---

## Task 8: ImageViewer + PdfViewer + выбор просмотрщика

**Files:**
- Create: `ui/src/components/workspace/binary-viewer.tsx`
- Modify: `ui/src/app/(authenticated)/workspace/page.tsx`
- Test: `ui/src/components/workspace/__tests__/binary-viewer.test.tsx` (Create)

**Interfaces:**
- Consumes: `WorkspaceFile`, `isBinaryFile` (Task 7).
- Produces: `<BinaryViewer file={...} onDownload={...} />` — рендерит `<img>` для `image/*`, `<iframe>` для `application/pdf`, иначе download-плашку.

- [ ] **Step 1: Написать падающий тест**

`ui/src/components/workspace/__tests__/binary-viewer.test.tsx`:

```tsx
import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { BinaryViewer } from "@/components/workspace/binary-viewer";

describe("BinaryViewer", () => {
  it("renders <img> for images", () => {
    render(<BinaryViewer file={{ is_binary: true, mime: "image/png", size: 1, url: "/workspace-files/x.png?sig=a", path: "x.png", is_dir: false }} />);
    const img = screen.getByRole("img");
    expect(img.getAttribute("src")).toBe("/workspace-files/x.png?sig=a");
  });

  it("renders iframe for pdf", () => {
    const { container } = render(<BinaryViewer file={{ is_binary: true, mime: "application/pdf", size: 1, url: "/workspace-files/d.pdf?sig=a", path: "d.pdf", is_dir: false }} />);
    expect(container.querySelector("iframe")?.getAttribute("src")).toBe("/workspace-files/d.pdf?sig=a");
  });
});
```

> Примечание: матчеры `@testing-library/jest-dom` (`toHaveAttribute`) НЕ подключены глобально (в `vitest.config.ts` нет `setupFiles`) — используем нативный `getAttribute()`.

- [ ] **Step 2: Запустить — падает**

Run: `cd ui && npx vitest run src/components/workspace/__tests__/binary-viewer.test.tsx`
Expected: FAIL — модуля нет.

- [ ] **Step 3: Реализовать**

`ui/src/components/workspace/binary-viewer.tsx`:

```tsx
"use client";

import type { WorkspaceFile } from "@/types/api";

type BinaryFile = Extract<WorkspaceFile, { is_binary: true }>;

export function BinaryViewer({ file }: { file: BinaryFile }) {
  if (file.mime.startsWith("image/")) {
    return (
      <div className="flex-1 min-h-0 flex items-center justify-center overflow-auto bg-background p-4">
        {/* eslint-disable-next-line @next/next/no-img-element */}
        <img src={file.url} alt={file.path} className="max-h-full max-w-full object-contain" />
      </div>
    );
  }
  if (file.mime === "application/pdf") {
    return <iframe src={file.url} title={file.path} className="flex-1 min-h-0 w-full border-0" />;
  }
  return (
    <div className="flex-1 flex flex-col items-center justify-center gap-3 text-muted-foreground">
      <span className="font-mono text-sm">{file.path}</span>
      <span className="text-xs">{(file.size / 1024).toFixed(1)} KB · {file.mime}</span>
      <a href={file.url} download className="text-primary underline text-sm">Скачать</a>
    </div>
  );
}
```

В `page.tsx`: заменить тип состояния и логику загрузки/рендера. Изменить `loadFile` чтобы хранить весь ответ; добавить состояние `fileData: WorkspaceFile | null`. Заменить блок рендера редактора:

```tsx
// imports
import { isBinaryFile } from "@/lib/api";
import type { WorkspaceFile } from "@/types/api";
import { BinaryViewer } from "@/components/workspace/binary-viewer";

// state (replace `content`/`original` usage for binary)
const [fileData, setFileData] = useState<WorkspaceFile | null>(null);

// in loadFile, after fetch:
const data = await apiGet<WorkspaceFile>(`/api/workspace/${filePath}`);
if (loadFileRequestRef.current !== requestId) return;
setSelectedFile(filePath);
setFileData(data);
if (!("is_binary" in data)) {
  setContent(data.content);
  setOriginal(data.content);
}
```

В JSX, где сейчас `isMarkdown ? <MarkdownEditor/> : <CodeEditor/>`:

```tsx
{fileData && isBinaryFile(fileData) ? (
  <BinaryViewer file={fileData} />
) : isMarkdown ? (
  <MarkdownEditor value={content} onChange={setContent} onSave={() => { if (isDirty) saveFile(); }} />
) : (
  <CodeEditor value={content} onChange={setContent} onSave={() => { if (isDirty) saveFile(); }} language={language} />
)}
```

Скрыть кнопку Save и индикатор «modified» когда `fileData` бинарный (Save имеет смысл только для текста). Обновить `setFileData(null)` в `navigateTo`/`navigateUp`/breadcrumb-сбросах рядом с `setContent("")`.

- [ ] **Step 4: Запустить — проходит**

Run: `cd ui && npx vitest run src/components/workspace/__tests__/binary-viewer.test.tsx`
Expected: PASS.

Run: `cd ui && npm run build`
Expected: сборка успешна (нет TS-ошибок).

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/workspace/binary-viewer.tsx ui/src/components/workspace/__tests__/binary-viewer.test.tsx "ui/src/app/(authenticated)/workspace/page.tsx"
git commit -m "feat(workspace-ui): image + PDF viewers, viewer selection by mime"
```

---

## Task 9: Файловые операции в дереве (папка/переименование/удаление/скачивание/загрузка)

**Files:**
- Modify: `ui/src/app/(authenticated)/workspace/page.tsx`
- Test: `ui/src/app/(authenticated)/workspace/__tests__/file-ops.test.tsx` (Create)

**Interfaces:**
- Consumes: `wsMkdir`, `wsRename`, `wsDeleteRecursive`, `wsUpload`, `signWorkspacePaths` (Task 7).
- Produces: чистый helper `function buildRenameTarget(currentPath: string, oldName: string, newName: string): { from: string; to: string }` (тестируем), + UI-обвязка.

- [ ] **Step 1: Написать падающий тест (чистый helper)**

`ui/src/app/(authenticated)/workspace/__tests__/file-ops.test.tsx`:

```tsx
import { describe, it, expect } from "vitest";
import { buildRenameTarget } from "@/app/(authenticated)/workspace/file-ops";

describe("buildRenameTarget", () => {
  it("keeps file in current folder", () => {
    expect(buildRenameTarget("zettelkasten/Note", "a.md", "b.md")).toEqual({
      from: "zettelkasten/Note/a.md",
      to: "zettelkasten/Note/b.md",
    });
  });
  it("works at root", () => {
    expect(buildRenameTarget("", "a.md", "b.md")).toEqual({ from: "a.md", to: "b.md" });
  });
});
```

- [ ] **Step 2: Запустить — падает**

Run: `cd ui && npx vitest run "src/app/(authenticated)/workspace/__tests__/file-ops.test.tsx"`
Expected: FAIL — модуля `file-ops` нет.

- [ ] **Step 3: Реализовать**

Create `ui/src/app/(authenticated)/workspace/file-ops.ts`:

```ts
/** Build absolute-within-workspace from/to paths for a rename within the current folder. */
export function buildRenameTarget(currentPath: string, oldName: string, newName: string) {
  const prefix = currentPath ? `${currentPath}/` : "";
  return { from: `${prefix}${oldName}`, to: `${prefix}${newName}` };
}
```

В `page.tsx` подключить операции:
- Кнопка «Новая папка» рядом с «Новый файл» в шапке `fileList` — инлайн-инпут (как `showNewFile`), по Enter зовёт `wsMkdir(currentPath ? \`${currentPath}/${name}\` : name)` → `fetchFiles()`.
- На каждом элементе дерева — иконки действий (показывать при hover): переименовать (инлайн-инпут → `wsRename(buildRenameTarget(...))` → `fetchFiles()`), скачать (для файла; для бинаря — `fileData.url`, для текста — получить URL через `signWorkspacePaths([path])` и `<a download>`), удалить.
- Удаление папки: `ConfirmDialog` с текстом «Папка “{name}” и всё её содержимое будут удалены безвозвратно.» → `wsDeleteRecursive(path)` → `fetchFiles()`/`navigateUp()`.
- Загрузка: скрытый `<input type="file" multiple>` + кнопка «Загрузить» и drag-drop по области дерева (`onDrop` → `wsUpload(currentPath, [...files])` → `fetchFiles()`).
- После переименования/удаления открытого файла: если `selectedFile` затронут — сбросить (`setSelectedFile(""); setFileData(null); setContent("")`).

```tsx
// imports
import { wsMkdir, wsRename, wsDeleteRecursive, wsUpload, signWorkspacePaths } from "@/lib/api";
import { buildRenameTarget } from "./file-ops";

// download helper
const downloadEntry = async (name: string) => {
  const path = currentPath ? `${currentPath}/${name}` : name;
  let url: string;
  if (fileData && selectedFile === path && isBinaryFile(fileData)) {
    url = fileData.url;
  } else {
    const map = await signWorkspacePaths([path]);
    url = map[path];
    if (!url) { setError("Не удалось подписать ссылку"); return; }
  }
  const a = document.createElement("a");
  a.href = url; a.download = name; a.click();
};
```

(Реализовать кнопки/инпуты в JSX по образцу существующего `showNewFile`. Стиль — как у текущих элементов дерева.)

- [ ] **Step 4: Запустить — проходит**

Run: `cd ui && npx vitest run "src/app/(authenticated)/workspace/__tests__/file-ops.test.tsx"`
Expected: PASS.

Run: `cd ui && npm run build`
Expected: успешно.

- [ ] **Step 5: Commit**

```bash
git add "ui/src/app/(authenticated)/workspace/"
git commit -m "feat(workspace-ui): folder create/rename/recursive-delete/download/upload"
```

---

**Фаза 2 завершена.** Картинки/PDF видны, файловый менеджер полноценный. Собрать UI и задеплоить статику (см. `reference_deploy_gaps`).

---

# Фаза 3 — CodeMirror Live Preview редактор для `.md`

Заменяет TipTap `MarkdownEditor` на CM6-редактор с декорациями. Каждая декорация — отдельная задача с чистыми тестируемыми хелперами + интеграция.

## Task 10: ObsidianEditor — каркас CM6 (lossless), замена MarkdownEditor

**Files:**
- Create: `ui/src/components/workspace/obsidian-editor.tsx`
- Modify: `ui/src/app/(authenticated)/workspace/page.tsx` (использовать ObsidianEditor вместо MarkdownEditor для `.md`)
- Modify: `ui/package.json` (добавить `@codemirror/language` в `dependencies`, если отсутствует явно)
- Test: `ui/src/components/workspace/__tests__/obsidian-editor.test.tsx` (Create)

**Interfaces:**
- Produces: `<ObsidianEditor value onChange onSave noteDir onNavigate />` — CM6 markdown, тема oneDark, Mod-S сохраняет, `onChange` отдаёт ровно текст буфера (lossless). `noteDir` — папка заметки для резолва картинок (Task 11); `onNavigate(target: string)` — для вики-ссылок (Task 12).

- [ ] **Step 1: Добавить явную зависимость**

```bash
cd ui && npm install @codemirror/language@^6
```

Expected: `@codemirror/language` в `dependencies`.

- [ ] **Step 2: Написать падающий тест (lossless round-trip)**

`ui/src/components/workspace/__tests__/obsidian-editor.test.tsx`:

```tsx
import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import { ObsidianEditor } from "@/components/workspace/obsidian-editor";

describe("ObsidianEditor", () => {
  // Lossless invariant: CM shows the raw markdown source verbatim (no WYSIWYG
  // transform), so frontmatter/callout markup is never mangled. `@testing-library/
  // user-event` is NOT installed and CM6 typing in jsdom is flaky — assert the
  // mounted DOM contains the raw source instead of simulating keystrokes.
  it("renders raw markdown source verbatim (no WYSIWYG transform)", () => {
    const src = "---\ntitle: x\n---\n\n# H\n\n> [!note]- T\n> line\n";
    const { container } = render(<ObsidianEditor value={src} onChange={() => {}} noteDir="" />);
    const content = container.querySelector(".cm-content");
    expect(content?.textContent).toContain("title: x");   // frontmatter shown as source
    expect(content?.textContent).toContain("[!note]-");   // callout markup verbatim
  });
});
```

- [ ] **Step 3: Запустить — падает**

Run: `cd ui && npx vitest run src/components/workspace/__tests__/obsidian-editor.test.tsx`
Expected: FAIL — модуля нет.

- [ ] **Step 4: Реализовать каркас**

`ui/src/components/workspace/obsidian-editor.tsx`:

```tsx
"use client";

import { useCallback, useRef, useEffect, useMemo } from "react";
import CodeMirror from "@uiw/react-codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { markdown } from "@codemirror/lang-markdown";
import { keymap, EditorView } from "@codemirror/view";

export interface ObsidianEditorProps {
  value: string;
  onChange: (v: string) => void;
  onSave?: () => void;
  /** Folder of the open note, workspace-relative — used to resolve relative image paths. */
  noteDir: string;
  /** Called when a [[wiki-link]] is clicked. */
  onNavigate?: (target: string) => void;
}

export function ObsidianEditor({ value, onChange, onSave, noteDir, onNavigate }: ObsidianEditorProps) {
  const onSaveRef = useRef(onSave);
  useEffect(() => { onSaveRef.current = onSave; }, [onSave]);

  const saveKeymap = useMemo(() => keymap.of([{
    key: "Mod-s",
    run: () => { onSaveRef.current?.(); return true; },
  }]), []);

  // Live Preview decoration extensions are appended here in Tasks 11-14.
  const extensions = useMemo(
    () => [markdown(), saveKeymap, EditorView.lineWrapping],
    [saveKeymap],
  );

  const handleChange = useCallback((v: string) => onChange(v), [onChange]);

  // noteDir / onNavigate are consumed by decoration extensions (Tasks 11-12).
  void noteDir; void onNavigate;

  return (
    <div className="flex-1 overflow-hidden">
      <CodeMirror
        value={value}
        onChange={handleChange}
        theme={oneDark}
        extensions={extensions}
        basicSetup={{ lineNumbers: false, foldGutter: false, highlightActiveLine: false }}
        className="h-full [&_.cm-editor]:h-full [&_.cm-scroller]:overflow-auto"
        height="100%"
      />
    </div>
  );
}
```

В `page.tsx`: импортировать `ObsidianEditor` (через `dynamic`, как `MarkdownEditor`) и заменить ветку `isMarkdown` на него. **Удалить теперь неиспользуемый `dynamic`-импорт `MarkdownEditor`** (иначе ESLint/`next build` ругнётся на unused import).

```tsx
const ObsidianEditor = dynamic(
  () => import("@/components/workspace/obsidian-editor").then((m) => m.ObsidianEditor),
  { ssr: false, loading: () => <div className="flex-1 animate-pulse bg-muted/20" /> },
);
// ...
) : isMarkdown ? (
  <ObsidianEditor
    value={content}
    onChange={setContent}
    onSave={() => { if (isDirty) saveFile(); }}
    noteDir={selectedFile.split("/").slice(0, -1).join("/")}
    onNavigate={(target) => { /* wired in Task 12 */ }}
  />
) : (
```

- [ ] **Step 5: Запустить — проходит**

Run: `cd ui && npx vitest run src/components/workspace/__tests__/obsidian-editor.test.tsx`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add ui/package.json ui/package-lock.json ui/src/components/workspace/obsidian-editor.tsx ui/src/components/workspace/__tests__/obsidian-editor.test.tsx "ui/src/app/(authenticated)/workspace/page.tsx"
git commit -m "feat(workspace-ui): CM6 ObsidianEditor scaffold (lossless), replaces TipTap for .md"
```

---

## Task 11: Инлайн-картинки с async-подписью

**Files:**
- Create: `ui/src/components/workspace/md-decorations/images.ts`
- Modify: `ui/src/components/workspace/obsidian-editor.tsx` (подключить расширение)
- Test: `ui/src/components/workspace/md-decorations/__tests__/images.test.ts` (Create)

**Interfaces:**
- Consumes: `signWorkspacePaths` (Task 7), `noteDir` (Task 10).
- Produces:
  - `function resolveAssetPath(noteDir: string, src: string): string | null` — резолвит относительный путь от папки заметки; `null` для абсолютных http(s).
  - `function findImageMatches(text: string): { from: number; to: number; src: string }[]` — все `![](...)`.
  - `imageDecorations(opts: { noteDir: string; getUrl: (path: string) => string | undefined }): Extension` — ViewPlugin, рисует `<img>` виджеты по видимой области.

- [ ] **Step 1: Написать падающий тест (чистые хелперы)**

`ui/src/components/workspace/md-decorations/__tests__/images.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { resolveAssetPath, findImageMatches } from "@/components/workspace/md-decorations/images";

describe("resolveAssetPath", () => {
  it("resolves relative against noteDir", () => {
    expect(resolveAssetPath("zettelkasten/Note", "images/x.png")).toBe("zettelkasten/Note/images/x.png");
  });
  it("returns null for absolute urls", () => {
    expect(resolveAssetPath("zettelkasten/Note", "https://e.com/a.png")).toBeNull();
  });
});

describe("findImageMatches", () => {
  it("finds standard markdown images", () => {
    const m = findImageMatches("text ![alt](images/x.png) more");
    expect(m).toHaveLength(1);
    expect(m[0].src).toBe("images/x.png");
  });
});
```

- [ ] **Step 2: Запустить — падает**

Run: `cd ui && npx vitest run src/components/workspace/md-decorations/__tests__/images.test.ts`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

`ui/src/components/workspace/md-decorations/images.ts`:

```ts
import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate, WidgetType } from "@codemirror/view";
import { type Extension, RangeSetBuilder } from "@codemirror/state";

const IMG_RE = /!\[[^\]]*\]\(([^)\s]+)\)/g;

export function resolveAssetPath(noteDir: string, src: string): string | null {
  if (/^https?:\/\//i.test(src) || src.startsWith("/")) return null;
  return noteDir ? `${noteDir}/${src}` : src;
}

export function findImageMatches(text: string): { from: number; to: number; src: string }[] {
  const out: { from: number; to: number; src: string }[] = [];
  for (const m of text.matchAll(IMG_RE)) {
    out.push({ from: m.index!, to: m.index! + m[0].length, src: m[1] });
  }
  return out;
}

class ImageWidget extends WidgetType {
  constructor(readonly url: string | undefined, readonly alt: string) { super(); }
  eq(o: ImageWidget) { return o.url === this.url; }
  toDOM() {
    const wrap = document.createElement("div");
    wrap.className = "cm-md-image";
    wrap.style.display = "block";
    if (this.url) {
      const img = document.createElement("img");
      img.src = this.url; img.alt = this.alt;
      img.style.maxWidth = "100%"; img.style.borderRadius = "6px";
      wrap.appendChild(img);
    } else {
      wrap.textContent = "🖼 …"; // placeholder until signed URL arrives
      wrap.style.opacity = "0.5";
    }
    return wrap;
  }
}

export function imageDecorations(opts: {
  noteDir: string;
  getUrl: (path: string) => string | undefined;
}): Extension {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) { this.decorations = this.build(view); }
      update(u: ViewUpdate) {
        if (u.docChanged || u.viewportChanged || u.selectionSet) this.decorations = this.build(u.view);
      }
      build(view: EditorView): DecorationSet {
        const b = new RangeSetBuilder<Decoration>();
        const cursor = view.state.selection.main.head;
        for (const { from, to } of view.visibleRanges) {
          const text = view.state.doc.sliceString(from, to);
          for (const m of findImageMatches(text)) {
            const mf = from + m.from, mt = from + m.to;
            if (cursor >= mf && cursor <= mt) continue; // show raw syntax on cursor line
            const resolved = resolveAssetPath(opts.noteDir, m.src);
            const url = resolved ? opts.getUrl(resolved) : m.src;
            // REPLACE the `![](...)` source range with the image widget (Live Preview:
            // hide markup, show render). NOT a trailing block widget — that would show
            // source AND image together.
            b.add(mf, mt, Decoration.replace({ widget: new ImageWidget(url, m.src) }));
          }
        }
        return b.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );
}
```

В `obsidian-editor.tsx` подключить с async-кэшем подписей через `StateField` + `StateEffect`:

```tsx
import { StateEffect, StateField } from "@codemirror/state";
import { imageDecorations, resolveAssetPath, findImageMatches } from "./md-decorations/images";
import { signWorkspacePaths } from "@/lib/api";

// URL cache field
const setUrls = StateEffect.define<Record<string, string>>();
const urlField = StateField.define<Record<string, string>>({
  create: () => ({}),
  update(value, tr) {
    for (const e of tr.effects) if (e.is(setUrls)) value = { ...value, ...e.value };
    return value;
  },
});
```

Внутри компонента: держать `viewRef` (через `onCreateEditor`/ref), при изменении `value`/`noteDir` собрать недостающие пути и подписать:

```tsx
const urlCacheRef = useRef<Record<string, string>>({});
const viewRef = useRef<EditorView | null>(null);

const ensureSigned = useCallback(async (doc: string) => {
  const need = new Set<string>();
  for (const m of findImageMatches(doc)) {
    const p = resolveAssetPath(noteDir, m.src);
    if (p && !urlCacheRef.current[p]) need.add(p);
  }
  if (!need.size) return;
  const map = await signWorkspacePaths([...need]);
  urlCacheRef.current = { ...urlCacheRef.current, ...map };
  viewRef.current?.dispatch({ effects: setUrls.of(map) });
}, [noteDir]);

useEffect(() => { ensureSigned(value); }, [value, ensureSigned]);
```

Добавить в `extensions`:

```tsx
urlField,
imageDecorations({ noteDir, getUrl: (p) => urlCacheRef.current[p] }),
```

и захватывать `view`:

```tsx
onCreateEditor={(view) => { viewRef.current = view; ensureSigned(value); }}
```

- [ ] **Step 4: Запустить — проходит**

Run: `cd ui && npx vitest run src/components/workspace/md-decorations/__tests__/images.test.ts`
Expected: PASS.

Run: `cd ui && npm run build`
Expected: успешно.

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/workspace/md-decorations/images.ts ui/src/components/workspace/md-decorations/__tests__/images.test.ts ui/src/components/workspace/obsidian-editor.tsx
git commit -m "feat(workspace-ui): inline image decorations with async signed URLs"
```

---

## Task 12: Вики-ссылки `[[...]]` + навигация

**Files:**
- Create: `ui/src/components/workspace/md-decorations/wikilinks.ts`
- Modify: `ui/src/components/workspace/obsidian-editor.tsx`, `ui/src/app/(authenticated)/workspace/page.tsx`
- Test: `ui/src/components/workspace/md-decorations/__tests__/wikilinks.test.ts` (Create)

**Interfaces:**
- Produces:
  - `function findWikiLinks(text: string): { from: number; to: number; target: string; label: string }[]` — `[[Note]]` и `[[Note#sec]]`.
  - `wikiLinkDecorations(onNavigate: (target: string) => void): Extension`.
- Consumes: `onNavigate` (Task 10).

- [ ] **Step 1: Написать падающий тест**

`.../wikilinks.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { findWikiLinks } from "@/components/workspace/md-decorations/wikilinks";

describe("findWikiLinks", () => {
  it("parses target and section", () => {
    const m = findWikiLinks("see [[My Note#Intro]] now");
    expect(m).toHaveLength(1);
    expect(m[0].target).toBe("My Note");
    expect(m[0].label).toBe("My Note#Intro");
  });
});
```

- [ ] **Step 2: Запустить — падает**

Run: `cd ui && npx vitest run src/components/workspace/md-decorations/__tests__/wikilinks.test.ts`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

`wikilinks.ts`:

```ts
import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate, WidgetType } from "@codemirror/view";
import { type Extension, RangeSetBuilder } from "@codemirror/state";

const WIKI_RE = /\[\[([^\]]+)\]\]/g;

export function findWikiLinks(text: string) {
  const out: { from: number; to: number; target: string; label: string }[] = [];
  for (const m of text.matchAll(WIKI_RE)) {
    const label = m[1];
    const target = label.split("#")[0].trim();
    out.push({ from: m.index!, to: m.index! + m[0].length, target, label });
  }
  return out;
}

class WikiWidget extends WidgetType {
  constructor(readonly label: string, readonly target: string, readonly onNavigate: (t: string) => void) { super(); }
  eq(o: WikiWidget) { return o.label === this.label; }
  toDOM() {
    const a = document.createElement("span");
    a.className = "cm-wikilink";
    a.textContent = this.label;
    a.style.cssText = "color:var(--primary,#7aa2f7);cursor:pointer;text-decoration:underline";
    a.onmousedown = (e) => { e.preventDefault(); this.onNavigate(this.target); };
    return a;
  }
}

export function wikiLinkDecorations(onNavigate: (target: string) => void): Extension {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(v: EditorView) { this.decorations = this.build(v); }
      update(u: ViewUpdate) {
        if (u.docChanged || u.viewportChanged || u.selectionSet) this.decorations = this.build(u.view);
      }
      build(view: EditorView): DecorationSet {
        const b = new RangeSetBuilder<Decoration>();
        const cursor = view.state.selection.main.head;
        for (const { from, to } of view.visibleRanges) {
          const text = view.state.doc.sliceString(from, to);
          for (const m of findWikiLinks(text)) {
            const mf = from + m.from, mt = from + m.to;
            if (cursor >= mf && cursor <= mt) continue;
            b.add(mf, mt, Decoration.replace({ widget: new WikiWidget(m.label, m.target, onNavigate) }));
          }
        }
        return b.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );
}
```

В `obsidian-editor.tsx`: добавить в `extensions` (передать стабильный колбэк):

```tsx
import { wikiLinkDecorations } from "./md-decorations/wikilinks";
// ...
wikiLinkDecorations((t) => onNavigate?.(t)),
```

В `page.tsx`: реализовать `onNavigate` — найти заметку по имени в текущей папке/vault и открыть:

```tsx
onNavigate={(target) => {
  const fname = target.endsWith(".md") ? target : `${target}.md`;
  // try current folder first
  loadFile(fname);
}}
```

- [ ] **Step 4: Запустить — проходит**

Run: `cd ui && npx vitest run src/components/workspace/md-decorations/__tests__/wikilinks.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/workspace/md-decorations/wikilinks.ts ui/src/components/workspace/md-decorations/__tests__/wikilinks.test.ts ui/src/components/workspace/obsidian-editor.tsx "ui/src/app/(authenticated)/workspace/page.tsx"
git commit -m "feat(workspace-ui): clickable [[wiki-links]] with navigation"
```

---

## Task 13: Callout'ы `> [!type]- Заголовок`

**Files:**
- Create: `ui/src/components/workspace/md-decorations/callouts.ts`
- Modify: `ui/src/components/workspace/obsidian-editor.tsx`
- Test: `ui/src/components/workspace/md-decorations/__tests__/callouts.test.ts` (Create)

**Interfaces:**
- Produces:
  - `function parseCalloutHeader(line: string): { type: string; collapsible: boolean; title: string } | null`
  - `calloutDecorations(): Extension` — line-decoration: красит строки блок-цитаты, начинающиеся с `[!type]`.

- [ ] **Step 1: Написать падающий тест**

`.../callouts.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { parseCalloutHeader } from "@/components/workspace/md-decorations/callouts";

describe("parseCalloutHeader", () => {
  it("parses type, collapsible and title", () => {
    expect(parseCalloutHeader("> [!note]- Полный транскрипт")).toEqual({
      type: "note", collapsible: true, title: "Полный транскрипт",
    });
  });
  it("non-callout returns null", () => {
    expect(parseCalloutHeader("> just a quote")).toBeNull();
  });
});
```

- [ ] **Step 2: Запустить — падает**

Run: `cd ui && npx vitest run src/components/workspace/md-decorations/__tests__/callouts.test.ts`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

`callouts.ts`:

```ts
import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate } from "@codemirror/view";
import { type Extension, RangeSetBuilder } from "@codemirror/state";

const HEADER_RE = /^>\s*\[!(\w+)\]([-+]?)\s*(.*)$/;

export function parseCalloutHeader(line: string) {
  const m = HEADER_RE.exec(line);
  if (!m) return null;
  return { type: m[1].toLowerCase(), collapsible: m[2] === "-" || m[2] === "+", title: m[3].trim() };
}

const headerDeco = Decoration.line({ class: "cm-callout-header" });
const bodyDeco = Decoration.line({ class: "cm-callout-body" });

export function calloutDecorations(): Extension {
  const plugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(v: EditorView) { this.decorations = this.build(v); }
      update(u: ViewUpdate) {
        if (u.docChanged || u.viewportChanged) this.decorations = this.build(u.view);
      }
      build(view: EditorView): DecorationSet {
        const b = new RangeSetBuilder<Decoration>();
        for (const { from, to } of view.visibleRanges) {
          let pos = from;
          while (pos <= to) {
            const line = view.state.doc.lineAt(pos);
            const text = line.text;
            if (parseCalloutHeader(text)) b.add(line.from, line.from, headerDeco);
            else if (text.startsWith(">")) b.add(line.from, line.from, bodyDeco);
            pos = line.to + 1;
          }
        }
        return b.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );
  const theme = EditorView.baseTheme({
    ".cm-callout-header": { borderLeft: "3px solid #7aa2f7", paddingLeft: "8px", fontWeight: "600", background: "rgba(122,162,247,0.08)" },
    ".cm-callout-body": { borderLeft: "3px solid #7aa2f7", paddingLeft: "8px", background: "rgba(122,162,247,0.04)" },
  });
  return [plugin, theme];
}
```

В `obsidian-editor.tsx` добавить `calloutDecorations()` в `extensions`.

- [ ] **Step 4: Запустить — проходит**

Run: `cd ui && npx vitest run src/components/workspace/md-decorations/__tests__/callouts.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/workspace/md-decorations/callouts.ts ui/src/components/workspace/md-decorations/__tests__/callouts.test.ts ui/src/components/workspace/obsidian-editor.tsx
git commit -m "feat(workspace-ui): Obsidian callout styling"
```

---

## Task 14: Frontmatter (regex) + базовая типографика

**Files:**
- Create: `ui/src/components/workspace/md-decorations/frontmatter.ts`
- Modify: `ui/src/components/workspace/obsidian-editor.tsx`
- Test: `ui/src/components/workspace/md-decorations/__tests__/frontmatter.test.ts` (Create)

**Interfaces:**
- Produces:
  - `function frontmatterRange(doc: string): { from: number; to: number } | null` — диапазон ведущего `---\n…\n---`.
  - `frontmatterDecorations(): Extension` — выделяет/стилизует блок свойств.

- [ ] **Step 1: Написать падающий тест**

`.../frontmatter.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { frontmatterRange } from "@/components/workspace/md-decorations/frontmatter";

describe("frontmatterRange", () => {
  it("detects leading frontmatter block", () => {
    const doc = "---\ntitle: x\ntags: [a]\n---\n\n# Body";
    const r = frontmatterRange(doc)!;
    expect(doc.slice(r.from, r.to)).toBe("---\ntitle: x\ntags: [a]\n---");
  });
  it("returns null when no frontmatter", () => {
    expect(frontmatterRange("# Just body")).toBeNull();
  });
});
```

- [ ] **Step 2: Запустить — падает**

Run: `cd ui && npx vitest run src/components/workspace/md-decorations/__tests__/frontmatter.test.ts`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

`frontmatter.ts`:

```ts
import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate } from "@codemirror/view";
import { type Extension, RangeSetBuilder } from "@codemirror/state";

const FM_RE = /^---\n[\s\S]*?\n---/;

export function frontmatterRange(doc: string): { from: number; to: number } | null {
  const m = FM_RE.exec(doc);
  if (!m || m.index !== 0) return null;
  return { from: 0, to: m[0].length };
}

export function frontmatterDecorations(): Extension {
  const plugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(v: EditorView) { this.decorations = this.build(v); }
      update(u: ViewUpdate) { if (u.docChanged) this.decorations = this.build(u.view); }
      build(view: EditorView): DecorationSet {
        const b = new RangeSetBuilder<Decoration>();
        const r = frontmatterRange(view.state.doc.toString());
        if (r) {
          const start = view.state.doc.lineAt(r.from).number;
          const end = view.state.doc.lineAt(r.to).number;
          for (let n = start; n <= end; n++) {
            const line = view.state.doc.line(n);
            b.add(line.from, line.from, Decoration.line({ class: "cm-frontmatter" }));
          }
        }
        return b.finish();
      }
    },
    { decorations: (v) => v.decorations },
  );
  const theme = EditorView.baseTheme({
    ".cm-frontmatter": { background: "rgba(128,128,128,0.10)", color: "#9aa5b1", fontStyle: "italic" },
  });
  return [plugin, theme];
}
```

В `obsidian-editor.tsx` добавить `frontmatterDecorations()` в `extensions`.

- [ ] **Step 4: Запустить — проходит**

Run: `cd ui && npx vitest run src/components/workspace/md-decorations/__tests__/frontmatter.test.ts`
Expected: PASS.

Run: `cd ui && npm test`
Expected: весь vitest зелёный.

Run: `cd ui && npm run build`
Expected: успешно.

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/workspace/md-decorations/frontmatter.ts ui/src/components/workspace/md-decorations/__tests__/frontmatter.test.ts ui/src/components/workspace/obsidian-editor.tsx
git commit -m "feat(workspace-ui): frontmatter block styling"
```

---

**Фаза 3 завершена.** `.md` рендерится как Obsidian-документ: картинки, `[[ссылки]]`, callout'ы, frontmatter — при сохранении без потерь.

---

# Финал

После всех фаз:
- `cargo test -p opex-core` + `cargo clippy -p opex-core --all-targets -- -D warnings` — зелёные.
- `cd ui && npm test` + `npm run build` — зелёные.
- Деплой: `make remote-deploy` (Rust) + сборка/синк UI-статики (см. `reference_deploy_gaps`).
- Прогнать security-reviewer на изменениях `workspace.rs` (path-traversal, upload-санитайз, recursive-delete-гард, sign-скоуп).
- E2E на сервере: открыть заметку из `zettelkasten/` с кадрами → картинки видны инлайн; открыть PDF; удалить непустую папку; загрузить файл; переименовать.
