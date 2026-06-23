import { TrendingUp, TrendingDown, Minus } from "lucide-react";

export function TableCard({ data }: { data: Record<string, unknown> }) {
  const title = data.title as string | undefined;
  const columns = (data.columns as string[]) ?? [];
  const rows = (data.rows as (string | number)[][]) ?? [];

  if (columns.length === 0) {
    return (
      <pre className="rounded-lg border bg-muted/30 p-4 text-sm font-mono whitespace-pre-wrap">
        {JSON.stringify(data, null, 2)}
      </pre>
    );
  }

  return (
    <div className="neu-flat overflow-hidden">
      {title && (
        <div className="border-b border-border/40 px-4 py-2.5">
          <span className="text-sm font-semibold text-foreground">{title}</span>
        </div>
      )}
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="border-b border-border/40 bg-muted/40">
              {columns.map((col, i) => (
                <th
                  key={i}
                  className="px-4 py-2.5 text-left font-mono text-xs font-semibold uppercase tracking-wider text-muted-foreground"
                >
                  {col}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, ri) => (
              <tr
                key={ri}
                className="border-b border-border/20 last:border-0 transition-colors hover:bg-muted/20"
              >
                {row.map((cell, ci) => (
                  <td key={ci} className="px-4 py-2 text-foreground/90">
                    {String(cell)}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

export function MetricCard({ data }: { data: Record<string, unknown> }) {
  const title = data.title as string | undefined;
  const value = data.value as string | undefined;
  const label = data.label as string | undefined;
  const trend = data.trend as "up" | "down" | "flat" | undefined;

  const TrendIcon =
    trend === "up" ? TrendingUp : trend === "down" ? TrendingDown : Minus;
  const trendColor =
    trend === "up"
      ? "text-success"
      : trend === "down"
        ? "text-destructive"
        : "text-muted-foreground";

  return (
    <div className="neu-flat inline-flex flex-col gap-1 px-5 py-4 overflow-hidden min-w-0">
      {title && (
        <span className="text-xs font-medium uppercase tracking-wider text-muted-foreground/70">
          {title}
        </span>
      )}
      <div className="flex items-baseline gap-3">
        <span className="text-3xl font-bold tabular-nums text-foreground break-all max-w-full">
          {value ?? "—"}
        </span>
        {trend && (
          <TrendIcon className={`h-5 w-5 ${trendColor}`} />
        )}
      </div>
      {label && (
        <span className="text-sm text-muted-foreground">{label}</span>
      )}
    </div>
  );
}
