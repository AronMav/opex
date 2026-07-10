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
