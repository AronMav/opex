import type { Locale } from "@/stores/language-store";
import type { Translations } from "./types";
import ru from "./locales/ru.json";
import en from "./locales/en.json";

const locales: Record<Locale, Translations> = {
  ru: ru as Translations,
  en: en as Translations,
};

export function getTranslations(locale: Locale): Translations {
  return locales[locale] ?? locales.en;
}
