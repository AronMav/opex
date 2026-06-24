# Checkpoint / Rollback

**Дата:** 2026-06-24
**Статус:** Design v1 (одобрено в brainstorming по 3 секциям; ожидает финального spec review)
**Hermes-референс:** `D:/GIT/hermes-agent` — `tools/checkpoint_manager.py` (~1670 строк): shared shadow-git store, `ensure_checkpoint` / `restore` / `list` / `diff` / `prune`, `DEFAULT_EXCLUDES`.

## Цель

Снапшотить файлы workspace **до** того, как агент начинает серию правок в ходе, и
давать пользователю откатить их при ошибке или по запросу. Вторая из трёх
Hermes-фич (после mid-run clarify, перед runtime user-hooks). Откат
инициирует **человек** (`/rollback`), не агент — агент не может «перемотать» себя.

## Контекст (как сейчас)

- Мутации файлов идут через системные tools: `workspace_write`, `workspace_edit`,
  `workspace_delete`, `workspace_rename` — их обработчики в
  `agent/pipeline/handlers.rs` (`handle_workspace_write:17`, `…_edit:133`,
  `…_delete:179`, `…_rename:195`). Все принимают `workspace_dir`, `agent_name`,
  `is_base`.
- Per-agent каталог workspace: `{workspace_dir}/agents/{agent_name}/` —
  `workspace.rs::agent_dir():57`. Часть каталогов (shared) живут в корне
  workspace, не под `agents/` (`validate_workspace_path_inner`, workspace.rs:641).
- Slash-команды разбираются в `agent/pipeline/commands.rs::handle_command:85`
  (`match command { "/status" | "/new" | "/reset" | "/compact" … }`); парсеры-
  образцы — `goal/mod.rs::parse_goal_command:57`. Результат команды возвращается
  как `command_output` (bootstrap.rs:51) и сразу эмитится без LLM-хода.
- `AppState` (gateway/state.rs:82) держит процесс-уровневые сервисы как
  `Arc<…>` (session_pools, tool_exec_ctx, audit_queue, …) — естественное место
  для одного `CheckpointManager` на процесс.
- Текущего механизма снапшотов/отката НЕТ (git workspace ≠ checkpoint: workspace
  не обязательно git-репо, и агент не должен коммитить в рабочий git проекта).

## Решения (brainstorming, 3 секции — все одобрены)

### Секция 1 — Storage / snapshot

1. **Shadow-git store** (полный Hermes-порт): отдельный **bare**-git репозиторий
   `~/.opex/checkpoints/store` (конфиг `store_path`), общий на все агенты.
   Workspace проекта НЕ трогается — снапшоты живут вне него.
2. **Изоляция через env, не через `cd`:** каждая git-операция выставляет
   `GIT_DIR=<store>`, `GIT_WORK_TREE={workspace_dir}/agents/{agent}`,
   `GIT_INDEX_FILE=<store>/index-{agent}` (порт Hermes). Это даёт:
   - один bare-store, но **отдельный index-файл на агента** (нет гонок индекса
     между параллельными агентами);
   - **отдельные refs на агента** — чекпойнты в `refs/checkpoints/{agent}/{n}`.
3. **Scope снапшота** = per-agent рабочий каталог `agents/{agent}/` + shared
   root-каталоги, которые агент может писать (определяются тем же списком, что
   `validate_workspace_path_inner`). За пределы scope снапшот не выходит.
4. **Excludes** (`DEFAULT_EXCLUDES`, порт Hermes + расширяемо конфигом): `.git`,
   `node_modules`, `target`, `dist`, `build`, `*.tmp`, `*.log`, бинарники и медиа
   (`*.png/jpg/mp3/mp4/zip/…`). Двойная роль: cost-guard (не раздуваем store) и
   safety (не снапшотим артефакты сборки).
5. **Триггер `ensure_checkpoint`** — лениво, **перед первой мутацией хода**
   (а не на каждый write): первый из `workspace_write/edit/delete/rename` в ходе
   снимает baseline-снапшот текущего состояния scope. Ходы без правок снапшот не
   создают. Граница хода отмечается `new_turn` (см. §интеграция).
6. **No-op при отсутствии изменений:** если рабочее дерево идентично последнему
   чекпойнту (тот же tree-hash), новый ref не создаётся.

### Секция 2 — Rollback / доступ / интеграция

7. **`/rollback` — slash-команда пользователя**, НЕ агентский tool. Подкоманды:
   - `/rollback` или `/rollback list` — список чекпойнтов (N, время, краткая
     сводка изменённых файлов);
   - `/rollback N` — откат всего scope к чекпойнту N;
   - `/rollback diff N` — показать diff текущее↔N без отката;
   - `/rollback N file <path>` — откат одного файла из чекпойнта N.
8. **`CheckpointManager` API** (порт Hermes, в `agent/checkpoint_manager.rs`):
   - `ensure_checkpoint(agent, workspace_dir) -> Result<Option<CheckpointId>>`
     (None = no-op, дерево не изменилось);
   - `list_checkpoints(agent) -> Vec<CheckpointMeta>`;
   - `restore(agent, workspace_dir, n, file: Option<&str>) -> Result<RestoreReport>`;
   - `diff(agent, workspace_dir, n) -> Result<String>`;
   - `new_turn(agent)` — отметить границу хода + ленивый prune (см. §3).
9. **Размещение:** один `Arc<CheckpointManager>` в `AppState`. Прокидывается в
   мутирующие handlers (через их deps/контекст) и в `CommandContext`
   (commands.rs) для `/rollback`.
10. **Валидация:**
    - commit/ref → `git rev-parse --verify <ref>^{commit}`; невалидный N →
      ошибка пользователю, без git-операции.
    - file-path при `restore … file` → нормализуется и проверяется, что внутри
      scope агента (anti-traversal), тем же подходом, что
      `workspace.rs::validate_workspace_path`.
11. **Forward-only откат:** `restore` применяет файлы в рабочее дерево и
    **создаёт новый чекпойнт** «restore of N» (история не переписывается) —
    откат можно откатить.

### Секция 3 — Prune / retention / конфиг / тестирование

12. **Retention per-agent — два независимых верхних предела, удаляем если нарушен
    ЛЮБОЙ:** count-cap `keep` (default 50) — оставить только последние `keep`
    чекпойнтов; age-cap `ttl_days` (default 14) — удалить старше `ttl_days`.
    Чекпойнт выживает ⟺ он среди последних `keep` И младше `ttl_days`; иначе
    prune. Удаление: `git update-ref -d refs/checkpoints/{agent}/{n}` +
    `git gc --auto` на store.
13. **Prune ленивый** — выполняется в `new_turn` (на входе в ход), без фонового
    таймера (меньше движущихся частей).
14. **Конфиг — секция `[checkpoint]` в `opex.toml`:**
    - `enabled = true` (kill-switch: на больших workspace shadow-git дорог);
    - `keep = 50`;
    - `ttl_days = 14`;
    - `store_path = "~/.opex/checkpoints/store"`;
    - `excludes = [...]` — поверх `DEFAULT_EXCLUDES`.
15. **Best-effort, не блокер:** любая git-ошибка (store недоступен, git не
    установлен, сбой) → логируем `warn`, ход агента **не** валится. Чекпойнт —
    подстраховка, не критический путь. `/rollback` при недоступном store →
    внятная ошибка пользователю.

## Non-goals (YAGNI)

- Branching чекпойнтов / дерево версий (только линейная история per-agent).
- UI-кнопка rollback — пока только `/rollback` (CLI/channel). UI — потом.
- Cross-agent shared чекпойнты (каждый агент изолирован своими refs/index).
- Durable через рестарт самого git-store не требует работы: store на диске —
  durable by default; in-memory только опциональный кэш метаданных.
- Снапшот вне workspace-scope (системные каталоги, config/, секреты) — никогда.
- Агентский tool для чекпойнта/отката — осознанно НЕ делаем (откат = действие
  человека).

## Компоненты

### 1. `CheckpointManager` (`agent/checkpoint_manager.rs`)

Порт `checkpoint_manager.py`. Состояние: `store_path`, `config`
(`keep`/`ttl_days`/`excludes`/`enabled`), опц. кэш `last_tree_hash` per-agent
для быстрого no-op. Все git-вызовы — через `tokio::process::Command` с env
`GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` (НЕ `cd`).

- **`ensure_store()`** — `git init --bare <store>` идемпотентно при первом
  использовании; настройка `gc.auto`.
- **`ensure_checkpoint(agent, workspace_dir)`**:
  1. если `!enabled` → `Ok(None)`.
  2. собрать env; `git add -A` с учётом excludes (через временный `.gitignore`
     в work-tree ИЛИ `git -c core.excludesFile=`); `git write-tree`.
  3. если tree-hash == последнему чекпойнту → `Ok(None)` (no-op).
  4. `git commit-tree` (или `git commit`) → новый commit; `git update-ref
     refs/checkpoints/{agent}/{next_n}`; вернуть `Some(id)`.
- **`list_checkpoints(agent)`** — `git for-each-ref refs/checkpoints/{agent}`
  + `git log --name-status` для сводки файлов на чекпойнт.
- **`restore(agent, workspace_dir, n, file)`**:
  - валидировать `n` (`rev-parse --verify`); при `file` — валидировать путь в
    scope;
  - `git checkout refs/checkpoints/{agent}/{n} -- <file|.>` в work-tree;
  - снять новый «restore of N» чекпойнт (forward-only); вернуть `RestoreReport`
    (список восстановленных файлов).
- **`diff(agent, workspace_dir, n)`** — `git diff refs/checkpoints/{agent}/{n}`.
- **`new_turn(agent)`** — отметка границы хода + `prune(agent)`.
- **`prune(agent)`** — удалить refs старше `keep`-го и старше `ttl_days`;
  `git gc --auto`.

### 2. Авто-чекпойнт в мутирующих handlers (`pipeline/handlers.rs`)

В начале `handle_workspace_write/edit/delete/rename` (перед самой мутацией)
вызвать `checkpoint_mgr.ensure_checkpoint(agent_name, workspace_dir).await` —
best-effort, ошибку только логировать. Ленивость даёт baseline ровно перед
первой правкой хода; повторные правки того же хода → no-op (дерево уже
снапшотнуто на первой). Нужно прокинуть `Arc<CheckpointManager>` в контекст
этих handlers.

### 3. `/rollback` (`pipeline/commands.rs`)

- Парсер `parse_rollback_command(args: &str) -> RollbackCmd` (образец
  `parse_goal_command`): `List | To(usize) | Diff(usize) | File(usize, String)`.
- Новый arm `"/rollback"` в `handle_command` рядом с `/compact`. Берёт
  `CheckpointManager` из `CommandContext` (добавить поле), `agent_name`,
  `workspace_dir`; форматирует ответ (локализованные строки, как прочие
  команды). Возврат — `Option<Result<String>>` → `command_output`.
- `CommandContext` расширяется ссылкой на `Arc<CheckpointManager>` +
  `workspace_dir` (если ещё не прокинут).

### 4. Конфиг (`config/opex.toml` + загрузчик)

- `[checkpoint]` секция, десериализуется в `CheckpointConfig`
  (`#[derive(Deserialize)]`, serde-defaults: `enabled=true`, `keep=50`,
  `ttl_days=14`, `store_path` с tilde-expand, `excludes` пустой → только
  `DEFAULT_EXCLUDES`).
- `CheckpointManager::new(config)` вызывается в main.rs при старте, кладётся в
  `AppState`.

### 5. `new_turn` хук

Вызывать `checkpoint_mgr.new_turn(agent)` на входе в ход — в `bootstrap` (начало
обработки сообщения). Это: (а) логическая граница «новый ход → можно
prune старое»; (б) сброс ленивого флага «baseline этого хода ещё не снят» (если
используем per-turn флаг вместо tree-hash сравнения). Реализация —
детализируется в плане; tree-hash no-op (§ensure_checkpoint п.3) делает
per-turn-флаг необязательным.

## Семантика (edge cases)

- **Параллельные агенты:** изоляция через отдельные `GIT_INDEX_FILE` и refs
  per-agent → один shared bare-store без гонок.
- **Workspace = не git / git проекта:** shadow-store полностью отдельный; рабочий
  git проекта не трогаем (work-tree указывает на каталог агента, GIT_DIR — на
  store, не на `.git` проекта).
- **No-op хода без правок:** `ensure_checkpoint` не зовётся (нет мутаций) ИЛИ
  возвращает None (дерево не изменилось) → store не растёт.
- **Откат к N, затем не нравится:** forward-only → есть «restore of N» чекпойнт +
  предыдущие; можно `/rollback` к состоянию до отката.
- **`restore … file <path>` вне scope:** reject (anti-traversal), без git-вызова.
- **Невалидный N:** `rev-parse --verify` fail → ошибка пользователю.
- **git/store недоступен:** ensure_checkpoint → warn, ход продолжается; rollback →
  ошибка пользователю.
- **excludes vs нужный файл:** excludes — только артефакты/бинарники; текстовые
  рабочие файлы агента под scope снапшотятся.

## Тестирование (TDD)

**Unit (`checkpoint_manager.rs`, требует git в PATH):**
- `ensure_checkpoint` на чистом scope создаёт ref `refs/checkpoints/{agent}/1`.
- повторный `ensure_checkpoint` без изменений файлов → `Ok(None)` (no-op, второй
  ref не создан).
- после правки файла `ensure_checkpoint` создаёт ref `/2`.
- `restore(n=1)` возвращает старое содержимое файла; создаёт forward-чекпойнт
  «restore of 1».
- `restore(n, file=X)` восстанавливает только X, прочие файлы не тронуты.
- `diff(n)` содержит ожидаемые изменения.
- `prune`: при `keep=2` третий чекпойнт сносит первый; TTL-ветка — чекпойнт
  старше `ttl_days` удаляется (время мокается/инжектится).
- excludes: файл из `DEFAULT_EXCLUDES` (напр. `node_modules/x`) не попадает в
  снапшот.

**Path-safety:**
- `restore(n, file="../../etc/passwd")` → reject, без git-операции.
- `rev-parse` отвергает мусорный N → ошибка, не паника.

**Integration:**
- `handle_workspace_write` на агенте триггерит `ensure_checkpoint` (мок/реальный
  store) перед записью; второй write того же хода → no-op.
- два агента пишут параллельно в shared-store → независимые refs/index, нет
  коллизий.
- `/rollback` парсинг: `list` / `2` / `diff 2` / `2 file notes/x.md` →
  корректные `RollbackCmd`.

**Best-effort:**
- ensure_checkpoint при `enabled=false` → `Ok(None)`, ход не затронут.
- ensure_checkpoint при недоступном store → `Err` логируется, handler
  возвращает обычный успех записи (ход не падает).

**Negative:**
- `/rollback` при пустой истории → внятное «нет чекпойнтов».
- `/rollback 99` (нет такого) → ошибка пользователю.

## Open questions / future

- UI-кнопка rollback (карточка с N-чекпойнтами).
- Авто-rollback при детекте провального хода (сейчас — только ручной).
- Branching/дерево версий.
- Конфигурируемый scope (сейчас фиксирован per-agent + shared root).
