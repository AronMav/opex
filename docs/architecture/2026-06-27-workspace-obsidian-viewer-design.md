# Workspace → полноценный Obsidian-просмотрщик и файловый менеджер

**Дата:** 2026-06-27
**Статус:** дизайн утверждён, готов к плану реализации
**Затрагивает:** `crates/opex-core/src/gateway/handlers/workspace.rs`,
`crates/opex-core/src/gateway/mod.rs` (регистрация роутов),
`crates/opex-core/src/gateway/handlers/workspace_files.rs` (переиспользование),
`ui/src/app/(authenticated)/workspace/page.tsx`, `ui/src/components/workspace/*`,
`ui/src/lib/api.ts`, `ui/src/types/api.ts`

## Проблема

Вкладка `/workspace/` в UI — слабый просмотрщик/редактор файлов. Конкретные дефекты,
заявленные пользователем:

1. **Картинки нельзя посмотреть.** `api_workspace_browse` читает любой файл через
   `tokio::fs::read_to_string` → бинарные файлы (PNG/JPG/PDF) падают с ошибкой UTF-8.
2. **PDF не открываются** по той же причине.
3. **`.md` рендерятся без картинок.** Редактор `.md` (TipTap WYSIWYG) не показывает
   встроенные изображения и не понимает Obsidian-конструкции.
4. **Нельзя удалить непустую папку.** `api_workspace_delete` зовёт `remove_dir` (только
   пустые), для непустой возвращает `409 Directory is not empty`.
5. Не хватает базовых файловых операций (создать папку, переименовать, загрузить, скачать).

### Контекст: что лежит в workspace

- `WORKSPACE_DIR = "workspace"`. Obsidian-vault `zettelkasten` лежит **внутри** workspace
  (`/workspace/zettelkasten`), obsidian-MCP монтирует `../workspace:/workspace`. Значит
  вкладка workspace **уже** показывает эти заметки — но плохо.
- Реальный формат заметок видео-пайплайна (`agent/file_scenario/video_summary.rs`):
  - **Стандартные markdown-картинки** с относительным путём: `![](images/frame.png)`
    (НЕ Obsidian-эмбеды `![[...]]`).
  - **Obsidian callout'ы**: `> [!note]- Полный транскрипт` (сворачиваемые).
  - **YAML-frontmatter**: `---\ntitle: …\ntags: [видео, конспект]\n---`.
  - Возможны **вики-ссылки** `[[Заметка#секция]]` (slug их экранирует → LLM их пишет).
  - Vault git-backed (`ops.js::commitVault`) — заметки агента ценны, порча недопустима.

## Принятые решения (из брейншторма)

- **Модель `.md`:** один живой редактор с инлайн-рендером (Obsidian Live Preview), без
  отдельного режима «чтение/правка».
- **Движок редактора:** CodeMirror 6 Live Preview (markdown-исходник — источник истины,
  декорации поверх). Причина — **сохранение без потерь**; альтернатива (TipTap WYSIWYG)
  отвергнута из-за риска уничтожить frontmatter (`---` трактуется как `<hr>`) и потерять
  семантику callout'ов при round-trip.
- **Файловые операции:** рекурсивное удаление папки + создать папку + переименовать +
  загрузка файлов + скачивание. Остальное (move drag-drop между папками и т.п.) — вне
  объёма (YAGNI).
- **PDF:** нативный `<iframe>` на подписанный URL (ноль новых зависимостей; зум/поиск/
  картинки — средствами браузера).

## Архитектура

Три слоя. Источник истины — каталог `workspace/`; все пути валидируются через существующий
`resolve_workspace_path` (canonicalize + `starts_with(base)` — защита от traversal/симлинков).

### Секция 1 — Backend: API файлов и отдача бинаря

Файл: `gateway/handlers/workspace.rs` (+ переиспользование `workspace_files.rs`,
`uploads::{mint_workspace_file_url, guess_mime_from_extension}`).

**Доступ к HMAC-ключу.** `api_workspace_browse` сейчас не берёт `State`. Для минта
подписанных URL хендлерам `browse` и `sign` нужен `State<InfraServices>` (как в
`workspace_files.rs`): ключ `infra.secrets.get_upload_hmac_key()`, TTL —
`config.uploads.signed_url_ttl_secs` (тот же источник, что в `file_scenarios/run.rs`).

**1.1. Бинарные файлы не падают.** В `api_workspace_browse` для файла:
- Определить бинарность по расширению; неизвестное — проба `std::str::from_utf8`
  прочитанных байт. Текстовые: `md/txt/json/toml/yaml/yml/csv/log/sh/rs/ts/js/py/…`.
  Бинарные/медиа: `png/jpg/jpeg/webp/gif/svg/pdf/…`. **`.svg` трактуем как изображение**
  (отдаём как бинарь с `image/svg+xml` → рендерится `<img>`; правка SVG-исходника — вне
  объёма).
- Текст → как сейчас: `{ "content": …, "path": …, "is_dir": false }`.
- Бинарь → `{ "is_binary": true, "mime": …, "size": …, "url": <signed> }`, где `url` —
  `mint_workspace_file_url(rel, &key, ttl)` → `/workspace-files/<rel>?sig=&exp=`.
  MIME — `uploads::guess_mime_from_extension`.
- **Корректность подписи (класс бага C-2):** минтить нужно тот же workspace-относительный
  путь, который `serve_workspace_file` затем `workspace_root.join(rel).canonicalize()`.
  Берём `rel = target_canonical.strip_prefix(base_canonical)`. Round-trip mint→verify→serve
  покрыть тестом (как существующий `roundtrip_mint_verify_resolve_for_agent_file`).

**1.2. Batch-подпись для инлайн-ассетов.** Новый авторизованный маршрут
`POST /api/workspace/sign`, тело `{ "paths": ["zettelkasten/Note/images/x.png", …] }` →
`{ "url_by_path": { "<path>": "/workspace-files/…?sig=&exp=" } }`.
- Каждый путь резолвится через `resolve_workspace_path`; пути вне workspace (и
  несуществующие) **молча отсутствуют** в ответе (не 4xx на весь батч).
- Минтится re-derived относительный путь (см. 1.1), чтобы подпись совпала с отдачей.
- Используется Live Preview редактором: все картинки одной заметки подписываются одним
  запросом, кэшируются на клиенте.

**1.3. Рекурсивное удаление.** `api_workspace_delete` (существующий роут
`DELETE /api/workspace/{*path}`) принимает query `?recursive=true`:
- `recursive=true` + директория → `tokio::fs::remove_dir_all`.
- Без флага — текущее поведение (`remove_dir`, для непустой `409`).
- Гард: запрет удаления самого корня workspace (`target == base_canonical` → `403`).
- Новый роут не нужен — только разбор query.

**1.4. Новые операции.** Чтобы НЕ конфликтовать с catch-all `/api/workspace/{*path}` (риск
паники matchit на старте при wildcard-сегментах-соседях), все новые POST-роуты — **простые
статические сегменты**, а целевой путь передаётся в теле/форме, не в URL-сегменте:
- `POST /api/workspace/sign` — см. 1.2.
- `POST /api/workspace/mkdir`, тело `{ "path": … }` → `create_dir_all`. Идемпотентно.
- `POST /api/workspace/rename`, тело `{ "from": …, "to": … }` → `tokio::fs::rename`. Оба
  конца резолвятся и обязаны быть внутри workspace; `to` не должен существовать (`409` при
  коллизии).
- `POST /api/workspace/upload` — `multipart/form-data`: текстовое поле `dir` (целевая
  папка, относительно workspace) + одно или несколько полей `file`. Каждый файл:
  `Path::file_name` (basename-санитайз, отказ при пустом/`..`), запись в `<dir>/<basename>`,
  лимит **50 MB** на файл. `dir` создаётся при отсутствии. Inbound-multipart в axum уже
  используется (`agents/icon.rs`).
  - **Body-limit:** дефолтный лимит тела axum — 2 MiB. Роут upload собрать **отдельным
    под-роутером** с `axum::extract::DefaultBodyLimit::max(...)` (≥ 50 MB + запас) и
    `.merge()`, ровно как `agents/icon.rs::routes()`.
- **Скачивание** — без нового эндпоинта. Фронт берёт подписанный `/workspace-files/`-URL
  (для бинаря — из ответа `browse`; для текста — через `POST /sign`) и кладёт в
  `<a download>`. Работает, т.к. UI и API за одним доменом (nginx) — same-origin.

Регистрация: статические роуты `/sign`, `/mkdir`, `/rename` — в основной
`workspace.rs::routes()` (статические соседи catch-all допустимы в matchit 0.8); `upload`
— отдельным под-роутером с собственным `DefaultBodyLimit`, затем `.merge()`.

### Секция 2 — Frontend: просмотрщики и файловые операции

Файл: `ui/src/app/(authenticated)/workspace/page.tsx` (+ новые компоненты в
`ui/src/components/workspace/`). Двухколоночный layout сохраняется.

**2.1. Выбор просмотрщика по ответу `browse`:**
- Ответ `is_binary` + `mime`:
  - `image/*` → новый `ImageViewer` (`<img>` по подписанному URL, центрирование,
    вписывание по контейнеру).
  - `application/pdf` → новый `PdfViewer` (`<iframe src={signedUrl}>`, во всю высоту).
  - иной бинарь → плашка «бинарный файл N KB» + кнопка «Скачать».
- Ответ с `content`:
  - `.md` → `ObsidianEditor` (секция 3).
  - прочий текст → текущий `CodeEditor` (без изменений).

Тип ответа отражается в `ui/src/types/api.ts` (расширить форму ответа workspace-файла).

**2.2. Файловые операции:**
- В заголовке файлового дерева: рядом с «Новый файл» — «Новая папка» (инлайн-инпут, как
  у файла; вызывает `mkdir`).
- На каждом элементе дерева — действия (иконки/контекстное меню): **Переименовать**
  (инлайн-инпут → `rename`), **Скачать** (бинарь и текст), **Удалить**.
- Удаление папки → `ConfirmDialog` с явной формулировкой «папка и всё её содержимое будут
  удалены безвозвратно» → `DELETE …?recursive=true`. Текущий путь после удаления — вверх.
- Загрузка: кнопка «Загрузить» + drag-drop поверх области дерева → `apiPostFormData` на
  `/api/workspace/upload` с полями `dir=<currentPath>` + `file` → рефреш списка.
  Используется существующий `apiPostFormData`.
- Переименование/удаление открытого файла обновляет `selectedFile` (или сбрасывает выбор).

### Секция 3 — Frontend: Obsidian Live Preview редактор для `.md`

Новый компонент `ui/src/components/workspace/obsidian-editor.tsx` на CM6. Зависимости уже
стоят: `@uiw/react-codemirror`, `@codemirror/view` (`Decoration`, `ViewPlugin`, `WidgetType`,
`StateField`/`StateEffect`), `@codemirror/lang-markdown`. `syntaxTree` живёт в
`@codemirror/language` (транзитивно через lang-markdown) — добавить его **явной**
зависимостью, чтобы импорт не сломался при изменении дерева пакетов. Новых тяжёлых
зависимостей нет.

**Инвариант:** markdown-исходник — единственный источник истины. `onChange`/`onSave` отдают
ровно текущий текст буфера. Декорации — только визуальный слой, не меняют сохраняемый текст.
Так round-trip невозможен в принципе → заметки агента не портятся.

**Декорации** (по модели Obsidian: на строке с курсором показываем сырой синтаксис,
вне — рендер). Реализация через `ViewPlugin` + `DecorationSet`. **Только видимая область:**
обходить `syntaxTree` в пределах `view.visibleRanges` (заметки с длинным callout-транскриптом
могут быть большими — полный обход тормозит). Конструкции:

1. **Инлайн-картинки** `![](relative/path)` — `WidgetType` с `<img>`. Относительный путь
   резолвится от папки открытой заметки (`dirname(selectedFile)`); абсолютные http(s)-URL
   берутся как есть.
   - **Async-подпись (важно):** декорации синхронны, а подписанные URL приходят
     асинхронно. Поток: собрать локальные пути → недостающие подписать батчем
     `POST /api/workspace/sign` → положить в кэш (`Map<path,url>` в `StateField`) → послать
     `StateEffect`, который обновит поле и заставит `ViewPlugin` перерисовать виджеты. Без
     этого первый рендер даст пустые картинки. До получения URL — плейсхолдер (скелетон).
2. **Вики-ссылки** `[[Заметка]]` / `[[Заметка#секция]]` — кликабельный виджет; клик зовёт
   колбэк навигации (родитель резолвит имя заметки в vault и переходит к ней). Несуществующая
   цель — приглушённый стиль.
3. **Callout'ы** `> [!type]- Заголовок` — обёртка-блок с иконкой/цветом по типу; суффикс
   `-` → сворачиваемый (свёрнут по умолчанию).
4. **Frontmatter** `---\n…\n---` в начале файла. `markdown()` по умолчанию НЕ выделяет
   frontmatter отдельным узлом (видит `---` как тематическую черту) → детектировать
   **регуляркой по началу документа** (ведущий блок `^---\n…\n---`) и декорировать этот
   диапазон как свёрнутый/выделенный блок «свойства»; не полагаться на `syntaxTree`.
5. **Базовая типографика** — заголовки, жирный/курсив, списки, инлайн-код, блоки кода —
   стилями CM (`HighlightStyle`/CSS), как Live Preview в Obsidian.

`Mod-S` сохраняет (как в текущих редакторах). Тёмная тема `oneDark` (консистентно с
`CodeEditor`).

## Поток данных

```
выбор файла → GET /api/workspace/<path>
   ├─ text   → { content }            → ObsidianEditor (.md) | CodeEditor (прочее)
   └─ binary → { is_binary, mime, url } → ImageViewer | PdfViewer | download-плашка

ObsidianEditor:
   парсит ![](…) → собирает локальные пути → POST /api/workspace/sign
   → url_by_path → <img> виджеты (кэш)

операции: mkdir / rename / upload / delete?recursive=true → рефреш дерева (fetchFiles)
```

## Обработка ошибок

- `browse` бинаря: не читаем содержимое в память для рендера (отдаём только URL+метаданные);
  фактическая выдача байт — через `/workspace-files/` с лимитами уже существующего хендлера.
- `sign`: пути вне workspace молча отсутствуют в `url_by_path` (не 4xx на весь батч).
- `upload`: превышение лимита → `413` (на уровне `DefaultBodyLimit` и/или ручной проверки
  размера); небезопасное имя → `400`; частичный успех батча — ответ перечисляет
  сохранённые/отклонённые.
- `rename`: коллизия (`to` существует) → `409`; пути вне workspace → `403`.
- `delete?recursive`: попытка удалить корень → `403`.
- Frontend: ошибки через существующий `ErrorBanner`; `<img>`/`<iframe>` `onError` →
  плашка «не удалось загрузить».

## Безопасность (для security-reviewer)

- **Path traversal:** все маршруты — через `resolve_workspace_path` (canonicalize +
  `starts_with`). `sign` дополнительно отбрасывает внешние пути.
- **Upload:** только basename (`file_name`), лимит 50 MB/файл, путь внутри workspace.
  Запись произвольных файлов в workspace — паритет с уже существующей записью текстовых
  файлов через `PUT`; новых классов риска не вводит.
- **Recursive delete:** деструктивно — гард на корень workspace, явный confirm в UI
  (опционально type-to-confirm имени папки против fat-finger). Уже сейчас вкладка позволяет
  удалять файлы; рекурсивное удаление папок — расширение того же права (админская вкладка за
  Bearer). Защиту base-агентских директорий не вводим (паритет с текущим поведением; при
  желании — отдельной задачей).
- **Body-limit upload:** дефолт axum 2 MiB перекрыть `DefaultBodyLimit` на под-роутере (см.
  1.4), иначе файлы >2 MB упрутся в дженерик-413 раньше ручной проверки.
- **Подписанные URL:** переиспользуют протестированный HMAC (`mint_/verify_workspace_file_url`).
  `/api/workspace/*` — за Bearer; `/workspace-files/*` — за HMAC+expiry (намеренно без Bearer,
  чтобы работать в `<img>`/`<iframe>`).
- **CSP:** сейчас заголовок `Content-Security-Policy-Report-Only` (observation mode, не
  блокирует), `img-src 'self' data: blob:`, `frame-src` отсутствует → наследует
  `default-src 'self'`. Same-origin `/workspace-files/` проходит и под текущей политикой, и
  под `'self'`. При будущем переключении на enforce убедиться, что `frame-src`/`object-src`
  не строже `'self'` (иначе PDF-`<iframe>` заблокируется).

## Тестирование (TDD)

**Backend (`cargo test`):**
- `browse` бинарного файла → `is_binary=true`, корректный MIME, валидный подписанный URL.
- `browse` UTF-8 текста → `content` без регрессии.
- `sign`: in-workspace путь подписывается; внешний путь отсутствует в ответе.
- `delete ?recursive=true` непустой папки → удалена; без флага → `409`; корень → `403`.
- `mkdir` идемпотентность; `rename` happy-path + коллизия `409` + traversal `403`.
- `upload`: сохранение + basename-санитайз + лимит размера.

**Frontend (`vitest`):**
- Выбор просмотрщика: `is_binary`+`mime` → image/pdf/прочий-бинарь; текст → `.md`
  (ObsidianEditor) vs прочее (CodeEditor) по расширению.
- `ObsidianEditor`: сохраняемый текст идентичен входному после набора (инвариант
  без-потерь); `![](…)` даёт запрос на подпись; `[[ссылка]]` зовёт навигацию; callout/
  frontmatter рендерятся.
- Файловые операции дёргают корректные эндпоинты и рефрешат дерево; confirm рекурсивного
  удаления.

## Последовательность сборки (одна спека, три фазы)

1. **Фаза 1 — Backend (секция 1).** Сразу чинит «картинки/PDF не открыть» и «непустую
   папку не удалить». Самостоятельная ценность.
2. **Фаза 2 — Frontend просмотрщики + файл-операции (секция 2).** Картинки/PDF видны,
   полноценный файловый менеджер.
3. **Фаза 3 — Obsidian Live Preview редактор (секция 3).** Самая объёмная; «полноценные
   Obsidian-документы».

Каждая фаза проверяема и ценна независимо. Деплой — по обычному `make remote-deploy` для
Rust; UI — отдельной сборкой (см. `reference_deploy_gaps`).

## Вне объёма (YAGNI)

- Перетаскивание файлов между папками, граф связей, поиск по vault, бэклинки-панель.
- `![[эмбеды]]` Obsidian-стиля (видео-пайплайн их не пишет; добавимо позже тем же
  механизмом виджетов, если понадобится).
- pdf.js (нативного iframe достаточно).
- Изменение формата хранения заметок или логики obsidian-MCP.
