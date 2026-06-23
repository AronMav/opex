"use client";

import { useState, useEffect } from "react";

export function MentionAutocomplete({ query, agents, onSelect }: {
  query: string;
  agents: string[];
  onSelect: (name: string) => void;
}) {
  const q = query.toLowerCase();
  const filtered = agents.filter(p => p.toLowerCase().startsWith(q));
  const [activeIdx, setActiveIdx] = useState(0);

  useEffect(() => { setActiveIdx(0); }, [query]);

  if (filtered.length === 0) return null;

  return (
    <div
      role="listbox"
      aria-label="Agent mentions"
      className="absolute bottom-full mb-1 left-0 max-h-[50vh] overflow-y-auto bg-popover border border-border rounded-lg shadow-lg p-1 z-50 w-full max-w-[280px]"
    >
      {filtered.map((name, i) => (
        <button
          key={name}
          role="option"
          aria-selected={i === activeIdx}
          className="flex items-center gap-2 px-3 py-1.5 text-sm rounded-md hover:bg-muted w-full text-left"
          onMouseDown={(e) => { e.preventDefault(); onSelect(name); }}
        >
          <span className="font-semibold">@{name}</span>
        </button>
      ))}
    </div>
  );
}
