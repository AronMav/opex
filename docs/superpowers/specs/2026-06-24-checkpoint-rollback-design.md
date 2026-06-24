# Checkpoint / Rollback

**Дата:** 2026-06-24
**Статус:** Design v2 (одобрено в brainstorming + адверсариальное ревью учтено; ожидает финального spec review)
**Hermes-референс:** `D:/GIT/hermes-agent` — `tools/checkpoint_manager.py` (~1670 строк): shared shadow-git store, env-isolation, commit-chain, `ensure_checkpoint` / `restore` / `list` / `diff` / `prune`, `_repair_bare_repo_dirs`, `DEFAULT_EXCLUDES`, size-guards.

## Цель

Снапшотить файлы workspace **до** того, как агент начинает серию правок в ходе, и
давать пользователю откатить их при ошибке или по запросу. Вторая из трёх
Hermes-фич (после mid-run clarify, перед runtime user-hooks). Откат
инициирует **человек** (`/rollback`), не агент — агент не может «перемотать» себя.

## Контекст (как сейчас — проверено по коду)

- Мутации файлов → системные tools: обработчики в `agent/pipeline/handlers.rs`
  (`handle_workspace_write:17`, `…_edit:133`, `…_delete:179`, `…_rename:195`).
  Все принимают `workspace_dir`, `agent_name`, `is_base`. Вызываются из
  `ToolDeps` (`agent/tool_registry.rs`), который несёт `cfg: &AgentConfig`,
  `state: &AgentState`, `session_id` — **handlers НЕ видят `AppState`**.
- Per-agent каталог: `{workspace_dir}/agents/{agent_name}/`
  (`workspace.rs::agent_dir():57`). Writable-scope шире (по
  `validate_workspace_path_inner`, workspace.rs:643+): кроме `agents/{agent}/`
  агент может писать в `tools/`, `skills/`, `mcp/`, `uploads/`, а base-агент —
  ещё в `toolgate/`, `channels/`. **Эти shared-каталоги общие на всех агентов и
  содержат чувствительное (config-подобное) — снапшотить их нельзя.**
- `AppState` (gateway/state.rs:82) — НЕ плоский: 6 cluster-структур
  (`agents/auth/infra/channels/config/status`). `AgentConfig.workspace_dir`
  существует (agent_config.rs:28). `AgentEngine` держит `state: Arc<AgentState>`
  и `cfg: Option<Arc<AgentConfig>>` (engine/mod.rs:52,55).
- Slash-команды: `agent/pipeline/commands.rs::handle_command:85`
  (`match command { "/status" | "/new" | "/reset" | "/compact" … }`). Его
  `CommandContext` (commands.rs:35) **не имеет** `workspace_dir`, НО несёт
  `engine_arc` (context_builder.rs:166) → `engine_arc.cfg()/state()`. Парсеры-
  образцы — `goal/mod.rs::parse_goal_command:57`. Результат → `command_output`
  (bootstrap.rs:51), эмитится без LLM-хода.
- Готовый anti-traversal: `agent/path_guard.rs` — `dunce::canonicalize` пути под
  `workspace_dir` (переиспользуем для restore-file валидации).
- Текущего механизма снапшотов/отката НЕТ.

## Ревью (адверсариальное) — что учтено в v2

CRIT: (1) work-tree лежит ВНУТРИ git-репо проекта → excludes только через
store-internal `$GIT_DIR/info/exclude` + `core.excludesFile`, **никогда** не
писать `.gitignore` в work-tree; (2) `AppState` не плоский и handlers видят
`ToolDeps`, не `AppState` → менеджер кладём в `AgentState`/`ToolDeps`; (3) у
slash-команд свой `CommandContext` без `workspace_dir` → берём из `engine_arc`.
HIGH: (4) scope сужен до ровно `agents/{agent}/` (shared-каталоги — non-goal);
(5) baseline-семантика хода прописана явно; (6) гонки `gc`/store на параллельных
агентах → per-store async Mutex + порт `_repair_bare_repo_dirs` + `gc.auto=0`.
MED: (7) tilde-expand — новый код/деп; (8) **single-ref + commit-chain** вместо
per-`n` refs (убирает гонку нумерации); (9) `reflog expire` перед `gc`;
(10) изоляция от пользовательского gitconfig (`GIT_CONFIG_*`, `--no-gpg-sign`);
(11) валидация charset `agent_name` перед интерполяцией в ref.
LOW: cross-platform git-in-PATH; `max_file_size_mb` guard; restore удаляет файлы,
созданные после чекпойнта (exact-tree); доп. тест-кейсы.

## Решения (итог)

### Storage / snapshot

1. **Shadow-git store** (Hermes-порт): отдельный **bare** репозиторий
   `~/.opex/checkpoints/store` (конфиг `store_path`, с tilde-expand), общий на
   всех агентов. Рабочий git проекта/workspace НЕ трогается.
2. **Изоляция через env, не `cd`.** Каждая git-операция выставляет:
   - `GIT_DIR=<store>`;
   - `GIT_WORK_TREE={workspace_dir}/agents/{agent}`;
   - `GIT_INDEX_FILE=<store>/index-{agent}` (отдельный index на агента);
   - `GIT_CONFIG_GLOBAL=<devnull>`, `GIT_CONFIG_SYSTEM=<devnull>`,
     `GIT_CONFIG_NOSYSTEM=1` (изоляция от `~/.gitconfig`: gpgsign/credential-
     helper не должны висеть/ломать фоновый снапшот);
   - `commit-tree`/`commit` всегда с `--no-gpg-sign` и фиксированными
     author/committer (env `GIT_AUTHOR_*`/`GIT_COMMITTER_*`), чтобы не зависеть
     от user.name/email.
3. **Scope снапшота = ровно `agents/{agent}/`** (work-tree). Shared-каталоги
   (`tools/`, `skills/`, `mcp/`, `uploads/`, base: `toolgate/`, `channels/`) и
   корень workspace **НЕ** снапшотятся (non-goal — они общие/чувствительные).
   Правки агента вне `agents/{agent}/` чекпойнтом не покрываются (документируем).
4. **Excludes** = `DEFAULT_EXCLUDES` (порт Hermes: `.git`, `node_modules`,
   `target`, `dist`, `build`, `*.tmp`, `*.log`, бинарники/медиа) + конфиг
   `excludes`. Применяются **только** через `$GIT_DIR/info/exclude` (или
   `core.excludesFile` на store-internal файл) — НИКОГДА записью в work-tree.
5. **`max_file_size_mb`** (default 5): перед `write-tree` файлы крупнее лимита
   убираются из index (`git rm --cached`), не попадают в снапшот (cheap-порт
   Hermes — excludes по расширению не ловят большие сгенерированные текстовики).
6. **Single ref + commit-chain на агента** (не per-`n` refs): один
   `refs/checkpoints/{agent}`; новый чекпойнт = `git commit-tree -p <tip>
   <tree>` затем `git update-ref refs/checkpoints/{agent} <new> <tip>` (CAS со
   старым значением — защита от гонки). «Чекпойнт N» = индекс в
   `git log refs/checkpoints/{agent}` (N=1 — самый свежий). Это чинит гонку
   нумерации и даёт корректные `git log`/`diff`/restore.
7. **Триггер `ensure_checkpoint`** — лениво, **перед первой мутацией хода**;
   no-op если `write-tree` == дереву текущего tip (ничего не изменилось).
8. **Baseline-семантика (явно):** baseline хода снимается ПЕРЕД первой правкой
   этого хода и фиксирует состояние scope на тот момент — **включая** любые
   внешние правки (другой агент, ручная правка, процесс), сделанные с прошлого
   хода. `/rollback N` возвращает scope к дереву чекпойнта N как есть. Параллельные
   правки в scope во время хода не сериализуются файловой системой — это
   осознанное ограничение (документируем + тест на drift между ходами).

### Rollback / доступ / интеграция

9. **`/rollback` — slash-команда пользователя**, НЕ агентский tool. Подкоманды:
   - `/rollback` или `/rollback list` — список (N, время, сводка файлов);
   - `/rollback N` — откат всего scope к чекпойнту N;
   - `/rollback diff N` — diff текущее↔N без отката;
   - `/rollback N file <path>` — откат одного файла из N.
10. **`CheckpointManager` API** (`agent/checkpoint_manager.rs`):
    - `ensure_checkpoint(agent, workspace_dir) -> Result<Option<CheckpointId>>`
      (None = no-op);
    - `list_checkpoints(agent) -> Vec<CheckpointMeta>` (newest-first, 1-based N);
    - `restore(agent, workspace_dir, n, file: Option<&str>) -> Result<RestoreReport>`;
    - `diff(agent, workspace_dir, n) -> Result<String>`;
    - `new_turn(agent)` — граница хода + ленивый prune.
11. **Размещение — один shared instance.** `CheckpointManager` строится в
    `main.rs` один раз и держит `Mutex<()>`-per-store (см. конкурентность);
    клонированный `Arc<CheckpointManager>` инжектится так, чтобы был
    **тот же** instance у всех агентов (иначе per-agent Mutex не сериализует
    store). Мутирующие handlers достают его из `ToolDeps` (через `state`/`cfg` —
    точное поле фиксируется планом, но instance общий). `/rollback` достаёт через
    `engine_arc` (cfg()/state()).
12. **Валидация:**
    - `agent_name` — только `[A-Za-z0-9_-]` перед интерполяцией в ref-путь
      (reject `/`, `..`, прочее);
    - N — `usize`; маппится в N-й коммит `git log`; вне диапазона → ошибка;
    - restore-file path → `path_guard` canonicalize под `agents/{agent}/`
      (anti-traversal, symlink-escape отлавливается canonicalize).
13. **Forward-only + exact-tree restore.** `restore` применяет дерево N в
    work-tree (`git checkout <commit> -- .`) И удаляет файлы, появившиеся после N
    (вычисляются `git diff --name-status --diff-filter=A <commit> <tip> -- .`,
    удаление в пределах scope) → дерево точно равно N. Затем снимается новый
    чекпойнт «restore of N» (история не переписывается — откат можно откатить).
    `restore … file <p>` восстанавливает только `<p>`.

### Retention / конфиг / конкурентность

14. **Retention per-agent — два независимых верхних предела, prune если нарушен
    ЛЮБОЙ:** count-cap `keep` (default 50) — оставить последние `keep`; age-cap
    `ttl_days` (default 14) — удалить старше. Чекпойнт выживает ⟺ среди последних
    `keep` И младше `ttl_days`. Реализация при commit-chain: «удалить» = перенести
    tip на N-й коммит и обрезать историю глубже (rewrite ref на сохранённый
    суффикс) ИЛИ оставить коммиты, но gc по `reflog expire`. Канон: prune
    переписывает `refs/checkpoints/{agent}` так, чтобы в истории осталось ≤`keep`
    коммитов не старше `ttl_days`, затем reflog-expire + gc собирает осиротевшие.
15. **Prune ленивый** — в `new_turn` (на входе в ход), без фонового таймера.
16. **Конкурентность.** Все **store-мутирующие** операции (`add`/`write-tree`/
    `commit-tree`/`update-ref`/`prune`/`gc`) сериализуются `tokio::sync::Mutex`
    на store внутри `CheckpointManager` (read-only `list`/`diff` могут идти без
    него). `gc.auto=0` (никакого неявного фонового gc); prune делает explicit
    `git reflog expire --expire=now --all` → `git gc --prune=now`. Перед
    операциями — порт `_repair_bare_repo_dirs` (Hermes:275): восстановить
    `refs/`, `branches/` если gc их снёс, иначе «fatal: not a git repository».
17. **Конфиг — `[checkpoint]` в `opex.toml`:** `enabled=true` (kill-switch),
    `keep=50`, `ttl_days=14`, `store_path="~/.opex/checkpoints/store"`,
    `excludes=[]` (поверх DEFAULT), `max_file_size_mb=5`. serde-defaults.
18. **Best-effort, не блокер.** Любая git-ошибка (store/ git недоступны, сбой) →
    `warn`, ход агента не валится. `/rollback` при недоступном store → внятная
    ошибка пользователю. **git обязан быть в PATH** на сервере (runtime-
    требование; при отсутствии — фича тихо no-op с warn в логах).

## Компоненты

### 1. `CheckpointManager` (`agent/checkpoint_manager.rs`)

Порт `checkpoint_manager.py`. Поля: `store_path`, `config`
(`enabled/keep/ttl_days/excludes/max_file_size_mb`), `store_lock:
tokio::sync::Mutex<()>`, опц. кэш `last_tip_tree` per-agent для быстрого no-op.
Все git-вызовы — `tokio::process::Command` с полным env-набором (§2). Хелпер
`run_git(agent, workspace_dir, args) -> Result<Output>` собирает env единообразно.

- **`ensure_store()`** — `git init --bare <store>` идемпотентно; `gc.auto=0`;
  записать `DEFAULT_EXCLUDES`+config в `<store>/info/exclude`.
- **`_repair_bare_repo_dirs()`** — создать недостающие `refs/`, `branches/`,
  `objects/`, `HEAD` (порт Hermes:275). Вызывается под lock перед мутациями.
- **`ensure_checkpoint(agent, workspace_dir)`** (под lock):
  1. `!enabled` → `Ok(None)`; репорнуть store/repair.
  2. `git add -A` (excludes из info/exclude); drop файлов > `max_file_size_mb`
     (`git rm --cached`); `git write-tree`.
  3. tree == tree(tip) → `Ok(None)` (no-op).
  4. `git commit-tree <tree> -p <tip> --no-gpg-sign -m "checkpoint"` →
     `git update-ref refs/checkpoints/{agent} <new> <tip>` (CAS). `Some(id)`.
- **`list_checkpoints(agent)`** — `git log --format=… --name-status
  refs/checkpoints/{agent}` для списка и сводки файлов (read-only).
- **`restore(agent, workspace_dir, n, file)`** (под lock): валидировать n/file →
  `git checkout <commit_n> -- <file|.>`; при полном restore удалить
  post-N-добавленные файлы (diff `--diff-filter=A`); снять «restore of N»
  чекпойнт; `RestoreReport`.
- **`diff(agent, workspace_dir, n)`** — `git diff <commit_n> -- .` (read-only).
- **`new_turn(agent)`** — `prune(agent)` (под lock).
- **`prune(agent)`** (под lock) — переписать ref на суффикс ≤`keep` и не старше
  `ttl_days`; `reflog expire --expire=now --all`; `gc --prune=now`.

### 2. Авто-чекпойнт в мутирующих handlers (`pipeline/handlers.rs`)

В начале `handle_workspace_write/edit/delete/rename` (до мутации):
`deps.<…>.checkpoint_mgr.ensure_checkpoint(agent_name, workspace_dir).await` —
best-effort, ошибку только `warn`. Ленивость → baseline ровно перед первой
правкой хода; повторные правки хода → no-op по tree-hash. `Arc<CheckpointManager>`
достаётся из `ToolDeps` (общий instance, §11).

### 3. `/rollback` (`pipeline/commands.rs`)

- Парсер `parse_rollback_command(args) -> RollbackCmd { List | To(usize) |
  Diff(usize) | File(usize, String) }` (образец `parse_goal_command`).
- Новый arm `"/rollback"` в `handle_command` рядом с `/compact`. Достаёт
  `CheckpointManager` + `workspace_dir` + `agent_name` из `engine_arc`
  (cfg()/state()); локализованный ответ; возврат `Option<Result<String>>`.

### 4. Конфиг (`config/opex.toml` + загрузчик)

- `[checkpoint]` → `CheckpointConfig` (`#[derive(Deserialize)]`, serde-defaults).
- tilde-expand `store_path`: `dirs::home_dir()` (новый деп, если отсутствует —
  отметить в плане) ИЛИ `$HOME`/`%USERPROFILE%` вручную (cross-platform).
- `CheckpointManager::new(config)` в main.rs → shared `Arc`, инжект в агентов.

### 5. `new_turn` хук (`pipeline/bootstrap.rs`)

Вызвать `checkpoint_mgr.new_turn(agent)` на входе в обработку сообщения (граница
хода → prune). Ленивый baseline снимается отдельно (§2), не здесь.

## Семантика (edge cases)

- **Параллельные агенты:** отдельные `GIT_INDEX_FILE`/refs per-agent + общий
  store-Mutex → нет гонок индекса/gc/object-write.
- **work-tree внутри git-репо проекта:** excludes только в `$GIT_DIR/info/exclude`
  → `git status` проекта не пачкается, ничего не коммитится в рабочий git.
- **gitconfig пользователя (gpgsign):** заглушен через `GIT_CONFIG_*` +
  `--no-gpg-sign` → фоновый снапшот не виснет/не падает.
- **Drift между ходами:** baseline = состояние на момент первой правки хода
  (§8); внешние правки до этого момента войдут в baseline и при откате
  «зафиксируются». Документируем; тест на drift.
- **Откат к N, затем не нравится:** forward-only → есть «restore of N» +
  предыдущие; можно откатить обратно.
- **Файлы, созданные после N:** exact-tree restore их удаляет (§13).
- **`restore … file` вне scope / symlink-escape:** reject через path_guard.
- **Невалидный N / `agent_name`:** ошибка, без git-вызова/без паники.
- **git/store недоступен:** ensure → warn, ход живёт; rollback → ошибка юзеру.

## Тестирование (TDD)

**Unit (`checkpoint_manager.rs`, git в PATH):**
- `ensure_checkpoint` на чистом scope создаёт `refs/checkpoints/{agent}` (1 коммит).
- повтор без изменений → `Ok(None)` (no-op, новый коммит не добавлен).
- после правки файла → новый коммит в цепочке (parent = прошлый tip).
- `restore(n=2)` (после 2 правок) возвращает старое содержимое; создаёт
  «restore of 2» коммит.
- `restore` удаляет файл, созданный ПОСЛЕ чекпойнта N (exact-tree).
- `restore(n, file=X)` восстанавливает только X.
- `diff(n)` содержит ожидаемые изменения.
- `prune`: `keep=2` → третий чекпойнт обрезает первый; `ttl_days` ветка
  (время инжектится) удаляет старое.
- excludes: `node_modules/x` и файл > `max_file_size_mb` не в снапшоте.

**Path/charset-safety:**
- `restore(n, file="../../etc/passwd")` → reject, без git-операции.
- `agent_name` с `/` или `..` → reject до ref-интерполяции.
- мусорный N → ошибка, не паника.

**Конкурентность/изоляция (порт-критичное):**
- work-tree внутри git-репо: после `ensure_checkpoint` `git status` проекта
  чист (info/exclude не попал в work-tree, ничего не застейджено в рабочем git).
- два агента пишут параллельно → независимые refs/index, store не повреждён.
- параллельный prune/gc под Mutex → store валиден (`_repair_bare_repo_dirs`
  восстанавливает структуру; `git fsck` ок).
- gitconfig с `commit.gpgsign=true` (мок env) → снапшот не виснет.

**Integration:**
- `handle_workspace_write` триггерит `ensure_checkpoint` перед записью; второй
  write того же хода → no-op.
- `/rollback` парсинг: `list` / `2` / `diff 2` / `2 file notes/x.md` →
  корректные `RollbackCmd`.
- drift: правка файла МЕЖДУ ходами входит в baseline следующего хода.

**Best-effort/negative:**
- `enabled=false` → `Ok(None)`, ход не затронут.
- недоступный store → `Err` логируется, write возвращает обычный успех.
- `/rollback` при пустой истории → «нет чекпойнтов»; `/rollback 99` → ошибка.

## Non-goals (YAGNI)

- Branching/дерево версий (только линейная commit-chain per-agent).
- UI-кнопка rollback (пока только `/rollback`).
- Снапшот вне `agents/{agent}/` (shared-каталоги, config/, секреты).
- Cross-agent shared чекпойнты.
- Агентский tool для чекпойнта/отката (откат = действие человека).
- Durable in-memory кэша через рестарт (store на диске durable by default).

## Open questions / future

- UI-карточка rollback со списком N.
- Авто-rollback при детекте провального хода (сейчас только ручной).
- Снапшот writable shared-каталогов, если появится потребность (сейчас non-goal).
