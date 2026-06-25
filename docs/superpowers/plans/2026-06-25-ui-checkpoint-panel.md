# Plan B — Checkpoint REST API + панель в чате

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`).

**Goal:** REST API для чекпойнтов (list/diff/restore поверх `CheckpointManager`) + панель в чате (Sheet: список/diff/откат).

**Architecture:** Backend — 3 хендлера под `/api/agents/{name}/checkpoints*`, резолв shared `CheckpointManager` из AppState (работает для стопнутых агентов), DTO `CheckpointListDto`/`CheckpointMetaDto`/`RestoreReportDto` (ts-rs). UI — `CheckpointPanel` (shadcn Sheet) из триггера в `ContextBar`, React Query.

**Tech Stack:** Rust (axum, serde, ts-rs за `ts-gen`), Next.js 16/React 19, shadcn (Sheet/Dialog/ConfirmDialog), React Query, vitest.

**Спека:** `docs/superpowers/specs/2026-06-25-ui-hooks-checkpoint-design.md` (v2, План B).

## Global Constraints

- **master**; без push; без `Co-Authored-By`; TDD. (make нет — прямые cargo; `cd ui` для npm. Backend-тесты гонять `cargo test --bin opex-core`, НЕ `--lib`.)
- **Резолв CheckpointManager — из AppState (shared `Arc<CheckpointManager>` в `AgentDeps.checkpoint_mgr`), НЕ через get_engine** (работает для стопнутых агентов). `workspace_dir` из `AgentDeps.workspace_dir`.
- GET checkpoints → `{enabled, items:[CheckpointMetaDto]}` (отличить disabled от пусто). diff/restore при `!enabled`/невалидном N → 4xx (из Err менеджера). `agent_name` валидируется (`schema::validate_agent_name` или charset).
- DTO: `CheckpointMetaDto {n, commit, created, summary}`, `RestoreReportDto {n, files, new_checkpoint}`, `CheckpointListDto {enabled, items}` — `#[derive(Serialize)]` + ts-rs за `ts-gen` + `register_ts_dto!`. min-count guard ui (после Plan A = 35) → +3 = 38.
- restore деструктивен → UI confirm-dialog обязателен.
- `relativeTime` util есть в `ui/src/lib/format.ts` — использовать (не Intl напрямую).

## File Structure

- Create `crates/opex-core/src/gateway/handlers/agents/checkpoints.rs` — 3 хендлера + DTO.
- Modify `crates/opex-core/src/gateway/handlers/agents/mod.rs` — routes.
- Modify `crates/opex-core/src/bin/gen_ts_types.rs` — min-count 35→38.
- Modify `ui/src/types/api.generated.ts` — codegen (не вручную).
- Modify `ui/src/lib/api.ts` — checkpoint fns.
- Modify `ui/src/lib/queries.ts` — qk + хуки.
- Create `ui/src/app/(authenticated)/chat/CheckpointPanel.tsx`.
- Modify `ui/src/app/(authenticated)/chat/ContextBar.tsx` — триггер.
- Test: DTO serde (Rust), vitest (api fns + CheckpointPanel).

---

### Task 1: Backend — checkpoint REST API + DTO

**Files:**
- Create: `crates/opex-core/src/gateway/handlers/agents/checkpoints.rs`
- Modify: `crates/opex-core/src/gateway/handlers/agents/mod.rs` (~19-30 routes)
- Modify: `crates/opex-core/src/bin/gen_ts_types.rs` (~50, min-count 35→38)
- Test: `#[cfg(test)]` в checkpoints.rs (DTO serde)

**Interfaces:**
- Consumes: `AgentDeps.checkpoint_mgr: Arc<CheckpointManager>` (state.rs); `CheckpointManager::{enabled, list_checkpoints, diff, restore}`; `CheckpointMeta {n,commit,created,summary}`, `RestoreReport {n,files,new_checkpoint}`.
- Produces: `CheckpointMetaDto`, `RestoreReportDto`, `CheckpointListDto {enabled, items}`; routes.

- [ ] **Step 1: Discovery — как хендлер достаёт checkpoint_mgr**

`AgentDeps` в AppState может быть за `RwLock` (`agent_deps: Arc<RwLock<AgentDeps>>`) ИЛИ извлекаться `State<AgentDeps>` (FromRef). Проверь по существующим хендлерам, как достаётся `AgentDeps`/`workspace_dir` (grep `agent_deps`/`AgentDeps` в gateway/handlers + state.rs FromRef-impl). Используй тот же путь (напр. `State<AppState>` → `state.<...>.agent_deps.read().await.checkpoint_mgr.clone()`). Зафиксируй найденный путь в отчёте. (Если `AgentDeps` НЕ FromRef — НЕ используй `State<AgentDeps>`.)

- [ ] **Step 2: Падающий тест — DTO serde**

В `#[cfg(test)]` checkpoints.rs:

```rust
#[test]
fn checkpoint_list_dto_serializes() {
    let dto = CheckpointListDto {
        enabled: true,
        items: vec![CheckpointMetaDto { n: 2, commit: "abc".into(), created: "2026-06-25T10:00:00+00:00".into(), summary: "1 file".into() }],
    };
    let j = serde_json::to_value(&dto).unwrap();
    assert_eq!(j["enabled"], true);
    assert_eq!(j["items"][0]["n"], 2);
    assert_eq!(j["items"][0]["created"], "2026-06-25T10:00:00+00:00");
}

#[test]
fn restore_report_dto_serializes() {
    let dto = RestoreReportDto { n: 1, files: vec!["a.md".into()], new_checkpoint: Some(3) };
    let j = serde_json::to_value(&dto).unwrap();
    assert_eq!(j["n"], 1);
    assert_eq!(j["new_checkpoint"], 3);
}
```

- [ ] **Step 3: FAIL**

Run: `cargo test --bin opex-core checkpoint_list_dto restore_report_dto -- --nocapture`
Expected: FAIL — DTO не найдены.

- [ ] **Step 4: Реализация**

`checkpoints.rs`:
```rust
use axum::{extract::{Path, State}, http::StatusCode, response::Json, routing::{get, post}, Router};
use serde::{Deserialize, Serialize};
use crate::gateway::state::AppState;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct CheckpointMetaDto { pub n: usize, pub commit: String, pub created: String, pub summary: String }
crate::register_ts_dto!(CheckpointMetaDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct CheckpointListDto { pub enabled: bool, pub items: Vec<CheckpointMetaDto> }
crate::register_ts_dto!(CheckpointListDto);

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-gen", ts(export))]
pub struct RestoreReportDto { pub n: usize, pub files: Vec<String>, pub new_checkpoint: Option<usize> }
crate::register_ts_dto!(RestoreReportDto);

#[derive(Deserialize)]
pub struct RestoreBody { #[serde(default)] pub file: Option<String> }
```

Хендлеры (резолв checkpoint_mgr + workspace_dir по Step 1 — пример с RwLock, адаптируй):
```rust
async fn api_list_checkpoints(State(state): State<AppState>, Path(name): Path<String>) -> Result<Json<CheckpointListDto>, StatusCode> {
    let (mgr, _ws) = resolve_mgr(&state).await;  // helper по Step 1
    let enabled = mgr.enabled();
    let items = mgr.list_checkpoints(&name).await.unwrap_or_default()
        .into_iter().map(|m| CheckpointMetaDto { n: m.n, commit: m.commit, created: m.created, summary: m.summary }).collect();
    Ok(Json(CheckpointListDto { enabled, items }))
}
async fn api_diff_checkpoint(State(state): State<AppState>, Path((name, n)): Path<(String, usize)>) -> Result<Json<serde_json::Value>, StatusCode> {
    let (mgr, ws) = resolve_mgr(&state).await;
    let diff = mgr.diff(&name, &ws, n).await.map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(serde_json::json!({ "diff": diff })))
}
async fn api_restore_checkpoint(State(state): State<AppState>, Path((name, n)): Path<(String, usize)>, Json(body): Json<RestoreBody>) -> Result<Json<RestoreReportDto>, StatusCode> {
    let (mgr, ws) = resolve_mgr(&state).await;
    let rep = mgr.restore(&name, &ws, n, body.file.as_deref()).await.map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(RestoreReportDto { n: rep.n, files: rep.files, new_checkpoint: rep.new_checkpoint }))
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents/{name}/checkpoints", get(api_list_checkpoints))
        .route("/api/agents/{name}/checkpoints/{n}/diff", get(api_diff_checkpoint))
        .route("/api/agents/{name}/checkpoints/{n}/restore", post(api_restore_checkpoint))
}
```
(`resolve_mgr(&state) -> (Arc<CheckpointManager>, String workspace_dir)` — реализуй по Step 1. `validate_agent_name` — вызови если есть pub-доступ; иначе manager сам валидирует внутри методов.)

В `agents/mod.rs` `routes()` добавить `.merge(checkpoints::routes())` + `mod checkpoints;`.

`gen_ts_types.rs`: min-count ui `35 → 38` (+3 DTO).

- [ ] **Step 5: PASS + codegen + сборка**

Run: `cargo test --bin opex-core checkpoint_list_dto restore_report_dto -- --nocapture` → PASS.
Run: `cargo run --bin gen_ts_types` → api.generated.ts содержит CheckpointMetaDto/CheckpointListDto/RestoreReportDto.
Run: `cargo check --all-targets` → clean.

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/agents/checkpoints.rs crates/opex-core/src/gateway/handlers/agents/mod.rs crates/opex-core/src/bin/gen_ts_types.rs ui/src/types/api.generated.ts
git commit -m "feat(ui-checkpoint): REST API list/diff/restore + DTO (резолв из AppState)"
```

---

### Task 2: UI — API + React Query хуки

**Files:**
- Modify: `ui/src/lib/api.ts`
- Modify: `ui/src/lib/queries.ts`
- Test: `ui/src/lib/__tests__/` (vitest) — api fns

**Interfaces:**
- Consumes: `CheckpointListDto`/`CheckpointMetaDto`/`RestoreReportDto` из `api.generated.ts` (Task 1 codegen).
- Produces: `listCheckpoints`/`diffCheckpoint`/`restoreCheckpoint` (api.ts); `useCheckpoints`/`useRestoreCheckpoint`/`useCheckpointDiff` + `qk.checkpoints` (queries.ts).

- [ ] **Step 1: Падающий vitest — api fns URL**

```typescript
import { describe, it, expect, vi } from "vitest";
// мокни apiFetch/fetch; проверь URL/метод
it("listCheckpoints бьёт в GET /api/agents/{name}/checkpoints", async () => {
  const spy = vi.fn().mockResolvedValue({ enabled: true, items: [] });
  // ... inject/mock apiGet
  await listCheckpoints("Agent");
  expect(spy).toHaveBeenCalledWith("/api/agents/Agent/checkpoints");
});
```
(Адаптируй под то, как мокаются api-fns в существующих ui-тестах; если apiGet нельзя замокать — тест на формирование URL через тонкую обёртку.)

- [ ] **Step 2: FAIL** → `cd ui && npx vitest run src/lib -t checkpoint`

- [ ] **Step 3: Реализация**

`api.ts`:
```typescript
export const listCheckpoints = (agent: string) =>
  apiGet<CheckpointListDto>(`/api/agents/${encodeURIComponent(agent)}/checkpoints`);
export const diffCheckpoint = (agent: string, n: number) =>
  apiGet<{ diff: string }>(`/api/agents/${encodeURIComponent(agent)}/checkpoints/${n}/diff`);
export const restoreCheckpoint = (agent: string, n: number, file?: string) =>
  apiPost<RestoreReportDto>(`/api/agents/${encodeURIComponent(agent)}/checkpoints/${n}/restore`, file ? { file } : {});
```

`queries.ts`:
```typescript
// qk: checkpoints: (name) => ["agents", name, "checkpoints"] as const
export function useCheckpoints(agent: string | null, enabled = true) {
  return useQuery({ queryKey: qk.checkpoints(agent ?? ""), queryFn: () => listCheckpoints(agent!), enabled: !!agent && enabled });
}
export function useRestoreCheckpoint() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ agent, n, file }: { agent: string; n: number; file?: string }) => restoreCheckpoint(agent, n, file),
    onSuccess: (_r, { agent }) => qc.invalidateQueries({ queryKey: qk.checkpoints(agent) }),
  });
}
```
(diff — по требованию: `useMutation`/прямой вызов `diffCheckpoint` в компоненте при клике.)

- [ ] **Step 4: PASS + tsc** → `cd ui && npx vitest run src/lib -t checkpoint` PASS; `npx tsc --noEmit` clean.

- [ ] **Step 5: Commit**

```bash
git add ui/src/lib/api.ts ui/src/lib/queries.ts ui/src/lib/__tests__/
git commit -m "feat(ui-checkpoint): api fns + React Query хуки (useCheckpoints/useRestoreCheckpoint)"
```

---

### Task 3: UI — CheckpointPanel + ContextBar триггер

**Files:**
- Create: `ui/src/app/(authenticated)/chat/CheckpointPanel.tsx`
- Modify: `ui/src/app/(authenticated)/chat/ContextBar.tsx`
- Test: `ui/src/app/(authenticated)/chat/__tests__/` (vitest)

**Interfaces:**
- Consumes: `useCheckpoints`/`useRestoreCheckpoint` (Task 2); `diffCheckpoint`; shadcn `Sheet`/`Dialog`/`ConfirmDialog`; `relativeTime` (`lib/format.ts`); `currentAgent` (chat-store).

- [ ] **Step 1: Падающий vitest — панель**

```tsx
it("CheckpointPanel рендерит список и пустое/disabled состояния", () => {
  // мок useCheckpoints → {enabled:true, items:[{n:2,created:"...",summary:"1 file",commit:"x"}]}
  render(<CheckpointPanel agent="Agent" open onOpenChange={()=>{}} />);
  expect(screen.getByText(/1 file/)).toBeInTheDocument();
  // enabled:false → "Чекпойнты отключены"; items:[] → "Чекпойнтов нет"
});
it("Откатить → confirm → restore mutation", async () => {
  // мок useRestoreCheckpoint; клик Откатить → confirm → mutate вызван с {agent, n}
});
```
(Адаптируй моки React Query под обёртки существующих chat-тестов.)

- [ ] **Step 2: FAIL** → `cd ui && npx vitest run "src/app/(authenticated)/chat" -t [Cc]heckpoint`

- [ ] **Step 3: Реализация**

`CheckpointPanel.tsx`: `Sheet` (props `agent`, `open`, `onOpenChange`). `useCheckpoints(agent, open)`. Состояния: loading; `!data.enabled` → «Чекпойнты отключены»; `data.items.length===0` → «Чекпойнтов нет»; иначе список строк. Строка: `#{n}` · `relativeTime(created)` · `summary`; кнопка **Diff** (вызов `diffCheckpoint(agent,n)` → `Dialog` с `<pre>` diff); кнопка **Откатить** → `ConfirmDialog` (variant destructive, описание «Откатит файлы агента к чекпойнту N») → `useRestoreCheckpoint().mutate({agent,n})` → `toast.success` + авто-invalidate.

`ContextBar.tsx`: добавить icon-button (история/rewind, напр. из lucide) рядом с model-badge → `onClick` открывает панель (локальный `useState(open)` в родителе чата ИЛИ в ContextBar; `currentAgent` из chat-store). Рендерить `<CheckpointPanel agent={currentAgent} open={open} onOpenChange={setOpen} />`.

- [ ] **Step 4: PASS + tsc + регрессия**

Run: `cd ui && npx vitest run "src/app/(authenticated)/chat" -t [Cc]heckpoint` → PASS.
Run: `cd ui && npx tsc --noEmit` → clean.
Run: `cd ui && npx vitest run "src/app/(authenticated)/chat"` → существующие chat-тесты целы.

- [ ] **Step 5: Commit**

```bash
git add ui/src/app/\(authenticated\)/chat/CheckpointPanel.tsx ui/src/app/\(authenticated\)/chat/ContextBar.tsx ui/src/app/\(authenticated\)/chat/__tests__/
git commit -m "feat(ui-checkpoint): CheckpointPanel (Sheet) + триггер в ContextBar"
```

---

## Self-Review

**1. Spec coverage:** REST list/diff/restore + резолв из AppState → Task 1; {enabled,items} → Task 1 (CheckpointListDto); DTO ts-rs → Task 1; api+хуки → Task 2; панель Sheet+confirm+relativeTime → Task 3; ContextBar триггер → Task 3. ✓
**2. Placeholder scan:** код в шагах; `resolve_mgr`/Step 1 discovery — как достать checkpoint_mgr (зависит от FromRef/RwLock, реальная проверка, не плейсхолдер); тест-моки помечены «адаптировать под обёртки».
**3. Type consistency:** `CheckpointMetaDto/CheckpointListDto/RestoreReportDto` идентичны Task 1 (Rust) ↔ Task 2/3 (TS из codegen). `qk.checkpoints` Task 2. `useCheckpoints/useRestoreCheckpoint` Task 2↔3. ✓
