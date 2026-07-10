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
