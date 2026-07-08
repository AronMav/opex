import { create } from "zustand";
import { devtools, persist, subscribeWithSelector } from "zustand/middleware";

export type Locale = "ru" | "en";

export const LOCALES: { value: Locale; label: string }[] = [
  { value: "ru", label: "Русский" },
  { value: "en", label: "English" },
];

interface LanguageState {
  locale: Locale;
  setLocale: (locale: Locale) => void;
}

function getInitialLocale(): Locale {
  // English is the default; a user's explicit choice (persisted below) wins.
  if (typeof window === "undefined") return "en";
  const stored = localStorage.getItem("opex.language");
  if (stored === "en" || stored === "ru") return stored;
  return "en";
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
