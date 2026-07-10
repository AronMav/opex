import type { CommandInfo } from "@/types/api";

export function CommandAutocomplete({
  input, commands, onPick,
}: { input: string; commands: CommandInfo[]; onPick: (name: string) => void }) {
  if (!input.startsWith("/")) return null;
  const q = input.slice(1).toLowerCase();
  const matches = commands.filter(
    (c) => c.name.toLowerCase().startsWith(q) || c.aliases.some((a) => a.toLowerCase().startsWith(q)),
  );
  if (matches.length === 0) return null;
  return (
    <div className="absolute bottom-full mb-1 w-full max-h-64 overflow-y-auto rounded-md border bg-popover shadow-md">
      {matches.map((c) => (
        <button key={c.name} type="button"
          className="flex w-full items-baseline gap-2 px-3 py-1.5 text-left hover:bg-accent"
          onClick={() => onPick(c.name)}>
          <span className="font-mono text-sm">/{c.name}</span>
          {c.args.length > 0 && (
            <span className="font-mono text-xs text-muted-foreground">
              {c.args.map((a) => `<${a.name}>`).join(" ")}
            </span>
          )}
          <span className="ml-auto truncate text-xs text-muted-foreground">{c.description}</span>
        </button>
      ))}
    </div>
  );
}
