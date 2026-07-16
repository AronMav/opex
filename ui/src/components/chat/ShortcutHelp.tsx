"use client";

import { useTranslation } from "@/hooks/use-translation";
import type { TranslationKey } from "@/i18n/types";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

interface ShortcutHelpProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

const SHORTCUT_GROUPS: Array<{
  titleKey: TranslationKey;
  shortcuts: Array<{ keys: string[]; descKey: TranslationKey }>;
}> = [
  {
    titleKey: "nav.chat" as TranslationKey,
    shortcuts: [
      { keys: ["Enter"], descKey: "chat.shortcut_send" as TranslationKey },
      { keys: ["Shift", "Enter"], descKey: "chat.shortcut_newline" as TranslationKey },
      { keys: ["Ctrl", "Shift", "N"], descKey: "chat.shortcut_new_chat" as TranslationKey },
      { keys: ["Ctrl", "Shift", "F"], descKey: "chat.shortcut_search" as TranslationKey },
      { keys: ["Ctrl", "K"], descKey: "chat.shortcut_palette" as TranslationKey },
      { keys: ["/"], descKey: "chat.shortcut_focus" as TranslationKey },
      { keys: ["Escape"], descKey: "chat.shortcut_stop" as TranslationKey },
      { keys: ["Ctrl", "/"], descKey: "chat.shortcut_help" as TranslationKey },
    ],
  },
];

function KeyBadge({ children }: { children: React.ReactNode }) {
  return (
    <kbd className="inline-flex items-center justify-center min-w-[24px] h-6 px-1.5 rounded border border-border bg-muted text-2xs font-mono font-medium text-muted-foreground">
      {children}
    </kbd>
  );
}

export function ShortcutHelp({ open, onOpenChange }: ShortcutHelpProps) {
  const { t } = useTranslation();

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent size="md">
        <DialogHeader>
          <DialogTitle>{t("chat.shortcuts_title")}</DialogTitle>
        </DialogHeader>
        <div className="space-y-4">
          {SHORTCUT_GROUPS.map((group) => (
            <div key={group.titleKey}>
              <h4 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground-subtle mb-2">
                {t(group.titleKey)}
              </h4>
              <div className="space-y-1.5">
                {group.shortcuts.map((s) => (
                  <div key={s.descKey} className="flex items-center justify-between py-1">
                    <span className="text-sm text-foreground/80">{t(s.descKey)}</span>
                    <div className="flex items-center gap-1">
                      {s.keys.map((k, i) => (
                        <span key={i} className="flex items-center gap-1">
                          {i > 0 && <span className="text-3xs text-muted-foreground/50">+</span>}
                          <KeyBadge>{k}</KeyBadge>
                        </span>
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      </DialogContent>
    </Dialog>
  );
}
