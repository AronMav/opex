export type ApiCommand = { name: string; description: string; scope?: string };

const TG_NAME_RE = /^[a-z0-9_]{1,32}$/;

/** Map registry commands to Telegram BotCommand shape; drop names Telegram rejects.
 *
 * Telegram's `BotCommand.description` must be 1-256 chars — an EMPTY description
 * makes `setMyCommands` reject the entire bulk batch, silently keeping every
 * command on its stale registration. A handler authored with a blank description
 * for the active language (Core `desc_for` returns "" verbatim) would trigger
 * that, so fall back to the command name when the description is empty (mirrors
 * Discord's `clampDesc`). */
export function commandsToTelegram(commands: ApiCommand[]): { command: string; description: string }[] {
  return commands
    .filter((c) => TG_NAME_RE.test(c.name))
    .map((c) => ({ command: c.name, description: ((c.description ?? "").trim() || c.name).slice(0, 256) }));
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
    if (!Array.isArray(body.commands)) return; // malformed body → leave stale menu
    const cmds = commandsToTelegram(body.commands);
    // Register the fetched set even when empty: a successful fetch that yields
    // zero commands is a legitimate "clear the menu" (e.g. all allowlist-gated
    // handlers disabled), distinct from a network/parse failure which returns
    // early above and leaves whatever menu Telegram already has.
    await bot.api.setMyCommands(cmds).catch(() => {});
  } catch {
    // fail-soft: leave whatever menu Telegram already has
  }
}
