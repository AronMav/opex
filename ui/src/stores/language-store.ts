import { create } from "zustand";
import { devtools, persist, subscribeWithSelector } from "zustand/middleware";
import { readWithLegacy } from "@/stores/ls-migration";

export type Locale = "ru" | "en";

export const LOCALES: { value: Locale; label: string }[] = [
  { value: "ru", label: "Русский" },
  { value: "en", label: "English" },
];

interface LanguageState {
  locale: Locale;
  setLocale: (locale: Locale) => void;
}

// Migrate legacy key so existing users keep their language preference.
function getInitialLocale(): Locale {
  if (typeof window === "undefined") return "ru";
  const migrated = readWithLegacy("opex.language", "hydeclaw.language");
  if (migrated === "en" || migrated === "ru") return migrated;
  return "ru";
}

export const useLanguageStore = create<LanguageState>()(
  devtools(
    subscribeWithSelector(
      persist(
        (set) => ({
          locale: getInitialLocale(),
          setLocale: (locale: Locale) => set({ locale }),
        }),
        { name: "opex.language" },
      ),
    ),
    { name: "LanguageStore", enabled: process.env.NODE_ENV !== "production" },
  ),
);
