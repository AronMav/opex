# Chat Command Registry — Фаза 2b (Telegram native commands) — план

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Telegram-меню «/» строится из реестра команд ядра (`GET /api/commands?scope=native`), а не из хардкод-списка — включая handler-команды (`/summarize_video`, `/transcribe`); статический список и channel-side `cmd*`-строки удаляются (F2/F5).

**Architecture:** Адаптер Telegram (Bun/TS) уже ходит в HTTP-API ядра (`OPEX_CORE_WS→http` + `OPEX_AUTH_TOKEN`, как для `/api/files/menu-run`). На старте драйвера вместо статического `bot.api.setMyCommands([...])` он фетчит `GET /api/commands?scope=native&lang=<L>`, маппит в `[{command, description}]` и регистрирует. Fail-soft: при ошибке фетча — не падаем (меню просто не обновится).

**Tech Stack:** TypeScript/Bun (`channels/`). Ядро не меняется (эндпоинт `/api/commands` уже отдаёт нужное с Фазы 2a).

**Спек:** [2026-07-09-chat-commands-registry-design.md](2026-07-09-chat-commands-registry-design.md) §Поверхности/Telegram (F2/F5). **Фаза 2a задеплоена** (origin/master 9b007d22): `/api/commands` возвращает `{commands:[{name,description,scope,...}], version}`, `?scope=native` фильтрует по `Native|Both`. Прод: 16 команд (14 builtin + transcribe + summarize_video).

## Global Constraints

- TypeScript/Bun; no new deps. Follow existing `channels/` patterns.
- Telegram native command names: `[a-z0-9_]{1,32}`. Все имена команд из реестра уже соответствуют (snake_case id/builtin-имена). Использовать `command.name` (каноническое), НЕ алиасы. Пропускать имена, не проходящие `/^[a-z0-9_]{1,32}$/` (защита от будущих несовместимых имён).
- Telegram-лимит: ≤100 команд в setMyCommands (у нас ~16 — с запасом).
- Fail-soft: ошибка фетча/сети не должна ронять драйвер (текущий код уже `.catch(() => {})`).
- HTTP к ядру: `const coreUrl = (process.env.OPEX_CORE_WS || "ws://localhost:18789").replace("ws://","http://"); const authToken = process.env.OPEX_AUTH_TOKEN || "";` (тот же паттерн, что у `hm:`-callback на telegram.ts:468-469).
- Тесты channels: `cd channels && bun test`. Деплой channels: push + `server-deploy.sh` (синкает `channels/src` + core restart re-spawns channels). Push/деплой — только с явного разрешения пользователя.
- Коммиты: 1/задача, без `Co-Authored-By`, master.

## Подтверждённые факты (существующий код)

- `channels/src/drivers/telegram.ts:199-208` — статический `bot.api.setMyCommands([{command,description}×7])` на `strings.cmd*`.
- `channels/src/drivers/telegram.ts:175-179` — драйвер получает `channelConfig` + `language`; `strings = getStrings(language)`.
- `channels/src/localization.ts:36-42` (интерфейс) + `:77-83` (RU) + EN-блок — поля `cmdHelp/cmdStatus/cmdMemory/cmdNew/cmdCompact/cmdStop/cmdThink`.
- `/api/commands?scope=native&lang=<L>` → `{"commands":[{"name":"...","description":"...","scope":"both",...}], "version":"..."}` (Фаза 2a).

---

## Файловая структура (2b)

**Модифицируется:**
- `channels/src/drivers/telegram.ts` — заменить статический `setMyCommands` на `registerCommandsFromRegistry()` (fetch + map + register).
- `channels/src/localization.ts` — удалить неиспользуемые `cmd*`-поля (интерфейс + все языковые блоки) после того, как telegram.ts перестанет их читать.
- `channels/src/drivers/telegram.test.ts` (или новый) — юнит-тест маппинга `commandsToTelegram(apiResponse)`.

**Создаётся (при отсутствии тест-файла):**
- `channels/src/drivers/telegram-commands.ts` — чистая функция `commandsToTelegram(commands)` (маппинг + фильтр валидных имён) — вынесена для юнит-тестируемости без сети.

---

## Task 1: Динамический `setMyCommands` из реестра + чистка хардкода (F2/F5)

**Files:**
- Create: `channels/src/drivers/telegram-commands.ts`
- Create: `channels/src/drivers/telegram-commands.test.ts`
- Modify: `channels/src/drivers/telegram.ts:199-208` (заменить статический список)
- Modify: `channels/src/localization.ts` (удалить `cmd*`-поля)

**Interfaces:**
- Produces:
  - `type ApiCommand = { name: string; description: string; scope?: string }`
  - `commandsToTelegram(commands: ApiCommand[]): { command: string; description: string }[]` — фильтрует по `/^[a-z0-9_]{1,32}$/`, обрезает description до 256 (Telegram-лимит), возвращает `{command:name, description}`.
  - `registerTelegramCommands(bot, coreUrl, authToken, language): Promise<void>` — фетчит `/api/commands?scope=native&lang=<language>`, маппит через `commandsToTelegram`, вызывает `bot.api.setMyCommands(...)`; fail-soft.

- [ ] **Step 1: Написать падающий тест маппинга**

`channels/src/drivers/telegram-commands.test.ts`:

```ts
import { describe, it, expect } from "bun:test";
import { commandsToTelegram } from "./telegram-commands";

describe("commandsToTelegram", () => {
  it("maps name+description and keeps valid names", () => {
    const out = commandsToTelegram([
      { name: "status", description: "Show status" },
      { name: "summarize_video", description: "Summarize a video" },
    ]);
    expect(out).toEqual([
      { command: "status", description: "Show status" },
      { command: "summarize_video", description: "Summarize a video" },
    ]);
  });

  it("drops names Telegram rejects (uppercase, hyphen, >32, empty)", () => {
    const out = commandsToTelegram([
      { name: "Status", description: "x" },     // uppercase
      { name: "export-session", description: "x" }, // hyphen
      { name: "a".repeat(33), description: "x" },   // too long
      { name: "", description: "x" },
      { name: "ok_cmd", description: "y" },
    ]);
    expect(out).toEqual([{ command: "ok_cmd", description: "y" }]);
  });

  it("truncates description to 256 chars", () => {
    const out = commandsToTelegram([{ name: "x", description: "d".repeat(300) }]);
    expect(out[0].description.length).toBe(256);
  });
});
```

- [ ] **Step 2: Прогнать — падает**

Run (из `channels/`): `bun test telegram-commands`
Expected: FAIL — `commandsToTelegram` не существует.

- [ ] **Step 3: Реализовать `telegram-commands.ts`**

```ts
export type ApiCommand = { name: string; description: string; scope?: string };

const TG_NAME_RE = /^[a-z0-9_]{1,32}$/;

/** Map registry commands to Telegram BotCommand shape; drop names Telegram rejects. */
export function commandsToTelegram(commands: ApiCommand[]): { command: string; description: string }[] {
  return commands
    .filter((c) => TG_NAME_RE.test(c.name))
    .map((c) => ({ command: c.name, description: (c.description ?? "").slice(0, 256) }));
}

/** Fetch the registry's native commands and register them with Telegram. Fail-soft. */
export async function registerTelegramCommands(
  bot: { api: { setMyCommands: (cmds: { command: string; description: string }[]) => Promise<unknown> } },
  coreUrl: string,
  authToken: string,
  language: string,
): Promise<void> {
  try {
    const resp = await fetch(
      `${coreUrl}/api/commands?scope=native&lang=${encodeURIComponent(language)}`,
      { headers: { Authorization: `Bearer ${authToken}` }, signal: AbortSignal.timeout(5000) },
    );
    if (!resp.ok) return;
    const body = (await resp.json()) as { commands?: ApiCommand[] };
    const cmds = commandsToTelegram(body.commands ?? []);
    if (cmds.length) await bot.api.setMyCommands(cmds).catch(() => {});
  } catch {
    // fail-soft: leave whatever menu Telegram already has
  }
}
```

- [ ] **Step 4: Прогнать — зелёные**

Run (из `channels/`): `bun test telegram-commands`
Expected: PASS (3 теста).

- [ ] **Step 5: Заменить статический список в telegram.ts + удалить cmd*-строки**

В `telegram.ts`:
1. Импорт: `import { registerTelegramCommands } from "./telegram-commands";`
2. Заменить блок `bot.api.setMyCommands([...])` (строки ~199-208) на:
   ```ts
   const coreUrl = (process.env.OPEX_CORE_WS || "ws://localhost:18789").replace("ws://", "http://");
   const authToken = process.env.OPEX_AUTH_TOKEN || "";
   void registerTelegramCommands(bot, coreUrl, authToken, language);
   ```
3. Grep telegram.ts для `strings.cmd` — убедиться, что после замены НИ ОДНА `cmd*`-строка больше не используется. (Единственное использование было в статическом списке.)

В `channels/src/localization.ts`:
4. Удалить поля `cmdHelp/cmdStatus/cmdMemory/cmdNew/cmdCompact/cmdStop/cmdThink` из интерфейса `Strings` (`:36-42`) И из КАЖДОГО языкового блока (RU `:77-83` + EN + все прочие). Grep `cmd` в localization.ts — не должно остаться ни одного (кроме несвязанных, если такие есть — не трогать).

- [ ] **Step 6: Полный channels-тест + typecheck**

Run (из `channels/`): `bun test` (весь набор зелёный) + `cd channels && bunx tsc --noEmit` (или существующий typecheck-скрипт) — 0 ошибок (докажет, что удаление `cmd*` не оставило висячих ссылок).

- [ ] **Step 7: Commit**

```bash
git add channels/src/drivers/telegram-commands.ts channels/src/drivers/telegram-commands.test.ts channels/src/drivers/telegram.ts channels/src/localization.ts
git commit -m "feat(channels): Telegram setMyCommands from command registry (drop hardcoded list)"
```

---

## Task 2: Интеграция, деплой, E2E

**Files:** нет новых.

- [ ] **Step 1: Полный channels-тест + core-паритет проверка**

Run (из `channels/`): `bun test`. Ожидание: зелёный.
Убедиться, что core уже отдаёт `?scope=native` (Фаза 2a, задеплоено): `curl -s -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:18789/api/commands?scope=native" | jq '.commands | length'` (на проде или после деплоя) — ≥16.

- [ ] **Step 2: Деплой (с разрешения пользователя)**

Push origin (ff) → `bash ~/opex-src/scripts/server-deploy.sh` (синкает `channels/src` + core restart re-spawns channels-адаптер, который на старте вызовет `registerTelegramCommands`).

- [ ] **Step 3: E2E в Telegram (живой клик — пользователь)**

В Telegram-клиенте открыть меню «/» бота: должны появиться команды из реестра, включая `summarize_video` и `transcribe` (handler-команды), а также builtin (`status`, `new`, `help`, `think`, …). Проверить, что `/summarize_video <url>` ставит задачу, `/help` показывает секцию обработчиков.

- [ ] **Step 4: Маркер завершения 2b**

```bash
git commit --allow-empty -m "chore(commands): Phase 2b complete — Telegram native commands from registry"
```

---

## Не в объёме 2b (осознанно отложено)

- **Telegram inline-кнопки для argsMenu** (`command_args_menu` → кнопки выбора): бэкенд пока НЕ эмитит option-меню — `try_handler_command` шлёт Menu только для отсутствующего источника (текст-prompt, который после 2a-фикса уже доставляется на канал как TextDelta). Кнопки станут нужны, когда добавим бэкенд-эмиссию argsMenu для choice-аргов (валвсов). → отдельный цикл (2c) вместе с бэкенд-эмиссией.
- **M1 (reuse `AppState.handlers`)**: сейчас `dispatch.rs` и `/help` конструируют свежий `HandlerRegistry` на каждое `/`-сообщение (полный toolgate-fetch, без ETag-reuse). Фикс = прокинуть общий `AppState.handlers` в движок (инвазивная проводка через EngineConfig/spawn — как Phase-1 P1). Перф-микроопт, fail-soft, localhost-loopback → низкий приоритет. → отдельная задача при желании.
- **Discord slash** — Фаза 3.
- **`<command>`-оверрайд** в toolgate-дескрипторе — при необходимости кастомных имён/алиасов.

## Self-Review (2b)

- **Покрытие:** динамический setMyCommands из `?scope=native` (T1 ✓), выпил хардкод-списка + `cmd*`-строк F2/F5 (T1 ✓), деплой + E2E (T2 ✓). Ядро не трогаем — API готов с 2a.
- **Плейсхолдеры:** нет. `commandsToTelegram` + `registerTelegramCommands` — полный код; тест — реальные ассерты (валидные/невалидные имена, обрезка описания). Один шаг требует «grep подтвердить, что `cmd*` больше не используется» — это верификация, не догадка; typecheck (Step 6) ловит висячие ссылки.
- **Согласованность типов:** `ApiCommand`/`commandsToTelegram`/`registerTelegramCommands` согласованы; форма ответа `/api/commands` (`{commands:[{name,description}]}`) совпадает с Фазой 2a.
- **Риск:** низкий — только channels TS, ядро без изменений, fail-soft фетч (ошибка не роняет драйвер и не ломает приём сообщений).
