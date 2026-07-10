export type ApiChoice = { value: string; label: string };
export type ApiArg = { name: string; description?: string; arg_type?: string; required?: boolean; menu?: boolean; choices?: { kind: string; values?: ApiChoice[] } };
export type ApiCommand = { name: string; description: string; scope?: string; args?: ApiArg[] };

export type DiscordOpt = { type: 3; name: string; description: string; required: boolean; choices?: { name: string; value: string }[] };
export type DiscordCmd = { name: string; description: string; options?: DiscordOpt[] };

const NAME_RE = /^[a-z0-9_-]{1,32}$/;
const clampDesc = (d: string, fallback: string) => {
  const s = (d ?? "").slice(0, 100);
  return s.length >= 1 ? s : fallback.slice(0, 100);
};

function argToOption(a: ApiArg, allowMenu: boolean): DiscordOpt | null {
  // menu:true args are choice-valves normally collected via the interactive
  // args-menu (/api/commands/menu-run). We drop them when the command ALSO has
  // a free-text arg (handler commands: source + valve) — exposing them there
  // would let their value get concatenated into the source URL. But when the
  // menu arg is the command's ONLY input (builtin /think, /voice), Discord has
  // no args-menu UI, so `allowMenu` lets it surface as a native choices
  // dropdown instead of being unsettable. A menu arg with no choices has
  // nothing to render, so it's still dropped.
  if (a.menu === true && !allowMenu) return null;
  if (!NAME_RE.test(a.name) || (a.arg_type && a.arg_type !== "string")) return null;
  const vals = a.choices?.values;
  if (a.menu === true && !(vals && vals.length)) return null;
  const opt: DiscordOpt = {
    type: 3, name: a.name, description: clampDesc(a.description ?? "", a.name), required: !!a.required,
  };
  if (vals && vals.length) {
    opt.choices = vals.slice(0, 25).map((c) => ({ name: (c.label ?? c.value).slice(0, 100), value: c.value.slice(0, 100) }));
  }
  return opt;
}

export function commandsToDiscord(commands: ApiCommand[]): DiscordCmd[] {
  return commands
    .filter((c) => NAME_RE.test(c.name))
    .map((c) => {
      const args = c.args ?? [];
      // If every arg is a menu-valve (no free-text arg to corrupt), allow the
      // valve(s) to render as native Discord choices — this is how builtin
      // /think and /voice stay usable on Discord (no args-menu UI there).
      const allowMenu = !args.some((a) => a.menu !== true);
      const options = args.map((a) => argToOption(a, allowMenu)).filter((o): o is DiscordOpt => o !== null).slice(0, 25);
      const cmd: DiscordCmd = { name: c.name, description: clampDesc(c.description, c.name) };
      if (options.length) cmd.options = options;
      return cmd;
    });
}

export function reconstructCommandText(commandName: string, values: Record<string, string>): string {
  // NOTE: positional flattening is safe only while a command exposes at most one
  // native option (every builtin has ≤1 arg; handler commands expose only their
  // single free-text `source`). Core's parse_command_line takes the first token
  // as the name and treats the rest as one opaque arg string, so there's no
  // multi-positional contract to preserve today. If a second native option is
  // ever added server-side, this must switch to declared-arg-order joining.
  const parts = Object.values(values).map((v) => (v ?? "").trim()).filter(Boolean);
  return parts.length ? `/${commandName} ${parts.join(" ")}` : `/${commandName}`;
}
