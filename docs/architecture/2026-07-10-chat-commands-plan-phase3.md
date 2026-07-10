# Chat Command Registry — Фаза 3 (Discord slash commands) — план

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Команды реестра появляются как нативные Discord slash-команды (с типизированными options + choices, где есть); использование slash-команды исполняется тем же ядром-диспатчем (как набранное `/cmd`), результат стримится в ответ Discord.

**Architecture:** На `ready` Discord-адаптер фетчит `GET /api/commands?scope=native&lang=<L>` (тот же endpoint, что Telegram в Ф.2b) и регистрирует команды через `client.application.commands.set(...)` — маппинг реестра в Discord `ApplicationCommandData` (name/description/options; string-options с choices из `arg.choices`). На `interactionCreate` (ChatInputCommand) адаптер `deferReply()`, реконструирует текст `/name <values>` из options и шлёт через существующий `bridge.sendMessage(dto)` (тот же путь, что `messageCreate`), стримя результат в `interaction.editReply()`.

**Tech Stack:** TypeScript/Bun, discord.js (`channels/`). Ядро НЕ меняется (`/api/commands` готов с Ф.2b, диспатч — с Ф.1/2a).

**Основа (задеплоено):** `channels/src/drivers/discord.ts` — `createDiscordDriver(bridge, credential, channelConfig, language, typingMode)`; `client` (discord.js `Client`, intents Guilds/GuildMessages/…); `messageCreate` строит `dto: IncomingMessageDto {user_id, channel, text, context:{guild_id, chat_id/channel_id, …}}` → `bridge.sendMessage(dto) → {requestId, onChunk, onPhase, result}` → стримит в `streamMsg` (edit). `/api/commands?scope=native&lang=` возвращает `{commands:[{name,description,scope,args:[{name,description,arg_type,required,choices?,…}]}], version}`. HTTP к ядру: `const coreUrl=(process.env.OPEX_CORE_WS||"ws://localhost:18789").replace("ws://","http://"); const authToken=process.env.OPEX_AUTH_TOKEN||"";` (как `registerTelegramCommands` в Ф.2b).

## Global Constraints

- TypeScript/Bun; discord.js уже в deps; без новых deps.
- **Discord naming:** command/option name = `^[-_\p{L}\p{N}]{1,32}$` и ДОЛЖНО быть lowercase (наши имена snake_case — проходят; фильтровать невалидные). Description 1..100 символов (обрезать/дефолт). ≤25 options на команду, ≤25 choices на option.
- Использовать `command.name` (каноническое), НЕ алиасы (Discord алиасов не имеет).
- Опции: `arg_type=="string"` → Discord `String` option (type 3); `required` из арга; `choices` из `arg.choices` (Static → `[{name,value}]`, ≤25); `capture_remaining`-арги → обычная string option. (MVP: number/boolean опции не строим — только string; если у команды нет валидных опций — команда без options.)
- Fail-soft: ошибка фетча/регистрации не роняет драйвер (как Telegram).
- **MVP:** slash-команда → реконструированный текст `/name <values>` → `bridge.sendMessage` → стрим результата. Discord-компоненты для `command_args_menu` (кнопки) — НЕ в объёме (future); backend argsMenu на Discord придёт текстом.
- Тесты: `cd channels && bun test`; typecheck `cd channels && bunx tsc --noEmit`. Деплой channels: push + `server-deploy.sh` (синк `channels/src` + core restart re-spawn адаптера). Push/деплой — только с явного разрешения. Discord E2E (живой бот/гильдия) — пользователь. Коммиты: 1/задача, без `Co-Authored-By`, master.

---

## Файловая структура (Ф.3)

**Создаётся:**
- `channels/src/drivers/discord-commands.ts` — `commandsToDiscord(commands) -> ApplicationCommandData-like[]` (чистый маппинг) + `reconstructCommandText(commandName, options)` (чистая реконструкция текста из option-значений).
- `channels/src/drivers/discord-commands.test.ts` — bun-тесты обоих.

**Модифицируется:**
- `channels/src/drivers/discord.ts` — `ready`: fetch `/api/commands` + `client.application.commands.set(commandsToDiscord(...))`; `interactionCreate`: defer + reconstruct + `bridge.sendMessage` + стрим в `editReply`.

---

## Task 1: `commandsToDiscord` + `reconstructCommandText` (чистые хелперы)

**Files:**
- Create: `channels/src/drivers/discord-commands.ts`
- Create: `channels/src/drivers/discord-commands.test.ts`

**Interfaces:**
- Produces:
  - `type ApiCommand = { name: string; description: string; scope?: string; args?: ApiArg[] }`, `type ApiArg = { name: string; description?: string; arg_type?: string; required?: boolean; choices?: { kind: string; values?: {value:string;label:string}[] } }`
  - `commandsToDiscord(commands: ApiCommand[]): DiscordCmd[]` where `DiscordCmd = { name: string; description: string; options?: DiscordOpt[] }`, `DiscordOpt = { type: 3; name: string; description: string; required: boolean; choices?: {name:string;value:string}[] }` — filters names to `^[a-z0-9_-]{1,32}$`, description clamped to 1..100 (fallback to name if empty), string options only, choices from `arg.choices.values` (≤25), ≤25 options.
  - `reconstructCommandText(commandName: string, values: Record<string,string>): string` — `"/" + name + (values joined by space in insertion order, non-empty)`.

- [ ] **Step 1: Падающий bun-тест**

`channels/src/drivers/discord-commands.test.ts`:

```ts
import { describe, it, expect } from "bun:test";
import { commandsToDiscord, reconstructCommandText } from "./discord-commands";

describe("commandsToDiscord", () => {
  it("maps a command with a choice arg to a String option with choices", () => {
    const out = commandsToDiscord([{
      name: "summarize_video", description: "Summarize a video",
      args: [{ name: "source", arg_type: "string", required: false },
             { name: "summary_length", arg_type: "string", required: false,
               choices: { kind: "static", values: [{value:"short",label:"short"},{value:"long",label:"long"}] } }],
    }]);
    expect(out).toEqual([{
      name: "summarize_video", description: "Summarize a video",
      options: [
        { type: 3, name: "source", description: "source", required: false },
        { type: 3, name: "summary_length", description: "summary_length", required: false,
          choices: [{ name: "short", value: "short" }, { name: "long", value: "long" }] },
      ],
    }]);
  });

  it("drops invalid names and clamps empty description to the name", () => {
    const out = commandsToDiscord([
      { name: "Bad Name", description: "x" },
      { name: "ok", description: "" },
    ]);
    expect(out).toEqual([{ name: "ok", description: "ok" }]);
  });
});

describe("reconstructCommandText", () => {
  it("joins name + non-empty values", () => {
    expect(reconstructCommandText("summarize_video", { source: "https://x/y", summary_length: "long" }))
      .toBe("/summarize_video https://x/y long");
  });
  it("bare command when no values", () => {
    expect(reconstructCommandText("status", {})).toBe("/status");
  });
});
```

- [ ] **Step 2: Прогнать — падает**

Run (из `channels/`): `bun test discord-commands` → FAIL (module missing).

- [ ] **Step 3: Реализовать `discord-commands.ts`**

```ts
export type ApiChoice = { value: string; label: string };
export type ApiArg = { name: string; description?: string; arg_type?: string; required?: boolean; choices?: { kind: string; values?: ApiChoice[] } };
export type ApiCommand = { name: string; description: string; scope?: string; args?: ApiArg[] };

export type DiscordOpt = { type: 3; name: string; description: string; required: boolean; choices?: { name: string; value: string }[] };
export type DiscordCmd = { name: string; description: string; options?: DiscordOpt[] };

const NAME_RE = /^[a-z0-9_-]{1,32}$/;
const clampDesc = (d: string, fallback: string) => {
  const s = (d ?? "").slice(0, 100);
  return s.length >= 1 ? s : fallback.slice(0, 100);
};

function argToOption(a: ApiArg): DiscordOpt | null {
  if (!NAME_RE.test(a.name) || (a.arg_type && a.arg_type !== "string")) return null;
  const opt: DiscordOpt = {
    type: 3, name: a.name, description: clampDesc(a.description ?? "", a.name), required: !!a.required,
  };
  const vals = a.choices?.values;
  if (vals && vals.length) {
    opt.choices = vals.slice(0, 25).map((c) => ({ name: c.label ?? c.value, value: c.value }));
  }
  return opt;
}

export function commandsToDiscord(commands: ApiCommand[]): DiscordCmd[] {
  return commands
    .filter((c) => NAME_RE.test(c.name))
    .map((c) => {
      const options = (c.args ?? []).map(argToOption).filter((o): o is DiscordOpt => o !== null).slice(0, 25);
      const cmd: DiscordCmd = { name: c.name, description: clampDesc(c.description, c.name) };
      if (options.length) cmd.options = options;
      return cmd;
    });
}

export function reconstructCommandText(commandName: string, values: Record<string, string>): string {
  const parts = Object.values(values).map((v) => (v ?? "").trim()).filter(Boolean);
  return parts.length ? `/${commandName} ${parts.join(" ")}` : `/${commandName}`;
}
```

- [ ] **Step 4: Прогнать — зелёные**

Run (из `channels/`): `bun test discord-commands` → PASS (4).

- [ ] **Step 5: Commit**

```bash
git add channels/src/drivers/discord-commands.ts channels/src/drivers/discord-commands.test.ts
git commit -m "feat(channels): map command registry to Discord slash commands (pure helpers)"
```

---

## Task 2: Регистрация + `interactionCreate` в discord.ts

**Files:**
- Modify: `channels/src/drivers/discord.ts`

**Interfaces:**
- Consumes: `commandsToDiscord`, `reconstructCommandText` (Task 1); `bridge.sendMessage(dto)` (existing); discord.js `Client`, `Interaction`.

- [ ] **Step 1: Регистрация на `ready`**

Импорт: `import { commandsToDiscord, reconstructCommandText } from "./discord-commands";` и из discord.js добавить `Events`, `ApplicationCommandDataResolvable`, `ChatInputCommandInteraction` по необходимости.

Добавить (после создания `client`, до `client.login`):

```ts
client.once("ready", async () => {
  try {
    const coreUrl = (process.env.OPEX_CORE_WS || "ws://localhost:18789").replace("ws://", "http://");
    const authToken = process.env.OPEX_AUTH_TOKEN || "";
    const resp = await fetch(`${coreUrl}/api/commands?scope=native&lang=${encodeURIComponent(language)}`,
      { headers: { Authorization: `Bearer ${authToken}` }, signal: AbortSignal.timeout(5000) });
    if (resp.ok) {
      const body = (await resp.json()) as { commands?: import("./discord-commands").ApiCommand[] };
      const cmds = commandsToDiscord(body.commands ?? []);
      if (cmds.length && client.application) {
        await client.application.commands.set(cmds as unknown as import("discord.js").ApplicationCommandDataResolvable[]).catch((e) => console.error("[discord] command register failed:", e));
      }
    }
  } catch (e) { console.error("[discord] command fetch failed:", e); }
});
```

- [ ] **Step 2: `interactionCreate` handler**

Добавить:

```ts
client.on("interactionCreate", async (interaction) => {
  if (!interaction.isChatInputCommand()) return;
  const userId = interaction.user.id;
  const { allowed, isOwner: _isOwner } = await bridge.checkAccess(userId);
  if (!allowed) {
    const code = await bridge.createPairingCode(userId, interaction.user.username);
    await interaction.reply({ content: strings.accessRestricted(code), ephemeral: true }).catch(() => {});
    return;
  }
  await interaction.deferReply().catch(() => {});
  // Reconstruct "/name <values>" from the provided options (in declared order).
  const values: Record<string, string> = {};
  for (const opt of interaction.options.data) {
    if (opt.value != null) values[opt.name] = String(opt.value);
  }
  const text = reconstructCommandText(interaction.commandName, values);
  const dto: IncomingMessageDto = {
    user_id: userId,
    channel: "discord",
    text,
    context: { guild_id: interaction.guildId, channel_id: interaction.channelId } as Record<string, unknown>,
  };
  let acc = "";
  const { onChunk, onPhase: _onPhase, result } = bridge.sendMessage(dto);
  onChunk((chunk: string) => {
    acc += chunk;
    // throttle-free minimal: edit with the accumulated text (Discord editReply)
    interaction.editReply(acc.slice(0, 2000) || "…").catch(() => {});
  });
  try {
    const final = await result;
    const out = (final && final.length ? final : acc) || "✓";
    await interaction.editReply(out.slice(0, 2000)).catch(() => {});
  } catch (err) {
    await interaction.editReply(strings.errorMessage((err as Error)?.message ?? "error")).catch(() => {});
  }
});
```

**Note:** adapt `IncomingMessageDto` field names + `bridge.sendMessage`'s exact return shape to match how `messageCreate` builds them (read the current `messageCreate` block — mirror its dto `context` keys and the `{requestId,onChunk,onPhase,result}` destructuring). If `onChunk`/`result`'s signature differs, match it. Discord message limit is 2000 chars — clamp.

- [ ] **Step 3: typecheck + bun test**

Run (из `channels/`): `bunx tsc --noEmit` (clean) + `bun test` (full green). Fix any discord.js type mismatches (e.g. `ApplicationCommandDataResolvable`, `ephemeral` deprecation → use `flags: MessageFlags.Ephemeral` if the installed discord.js requires it — check `channels/package.json` discord.js version).

- [ ] **Step 4: Commit**

```bash
git add channels/src/drivers/discord.ts
git commit -m "feat(channels): register Discord slash commands + interaction dispatch to core"
```

---

## Task 3: Интеграция, деплой, E2E

- [ ] **Step 1:** `cd channels && bun test` (green) + `bunx tsc --noEmit` (clean).
- [ ] **Step 2:** Деплой (с разрешения): push → `server-deploy.sh` (синк `channels/src` + core restart re-spawn Discord-адаптера, который на `ready` зарегистрирует slash-команды).
- [ ] **Step 3:** E2E (живой Discord — пользователь): в гильдии/DM бота ввести `/` → появляются команды реестра (builtin + `/summarize_video` с опцией `source` + choice `summary_length`); использовать `/summarize_video source:<url> summary_length:long` → job ставится; `/status` → ответ статуса. (Глобальная регистрация Discord пропагируется до ~1ч; per-guild — мгновенно. Если долго — упомянуть.)
- [ ] **Step 4:** Маркер: `git commit --allow-empty -m "chore(commands): Phase 3 complete — Discord slash commands"`.

---

## Не в объёме Ф.3

- Discord-компоненты (Buttons/SelectMenu) для `command_args_menu` argsMenu — future (Discord MVP шлёт argsMenu текстом).
- number/boolean Discord options — MVP только string.
- Стриминг-троттлинг Discord editReply (MVP редактирует по чанку; при необходимости добавить throttle позже).

## Self-Review (Ф.3)

- **Покрытие:** маппинг реестра→Discord + реконструкция (T1 ✓), регистрация на ready + interaction→bridge→стрим (T2 ✓), деплой+E2E (T3 ✓). Ядро не трогаем — `/api/commands` готов.
- **Плейсхолдеры:** нет. T2 помечает «сверить dto-поля/сигнатуру bridge с messageCreate» — конкретная верификация (mirror существующего блока), не догадка; typecheck ловит несоответствия discord.js.
- **Согласованность:** `commandsToDiscord`/`reconstructCommandText`/`ApiCommand`/`DiscordCmd` согласованы; форма ответа `/api/commands` совпадает с Ф.2b.
- **Риск:** средний — только channels TS, ядро без изменений; регистрация/фетч fail-soft; discord.js версия может требовать `flags` вместо `ephemeral` (typecheck ловит).
