"use client";

import { useCallback } from "react";
import { useLanguageStore } from "@/stores/language-store";
import { getTranslations } from "@/i18n";
import type { TranslationKey } from "@/i18n/types";

type InterpolationValues = Record<string, string | number>;

/**
 * Resolve the string to interpolate for `key`. When the caller passes a numeric
 * `count`, prefer the CLDR plural-variant key (`<key>_one`/`_few`/`_many`/
 * `_other`) chosen by `Intl.PluralRules` for the active locale, falling back to
 * the base `<key>` when no matching variant exists. Callers that pass no
 * `count` (or a `key` with no `_one`/`_few`/… variants) resolve to the base
 * `<key>` exactly as before — this path is fully backward-compatible.
 */
function resolveText(
  translations: Record<string, string>,
  locale: string,
  key: TranslationKey,
  values?: InterpolationValues,
): string {
  const count = values?.count;
  if (typeof count === "number") {
    const category = new Intl.PluralRules(locale).select(count);
    const variant = translations[`${key}_${category}`];
    if (variant !== undefined) return variant;
    // Intl may pick "few"/"many" but the locale only defines "other" — try it.
    const other = translations[`${key}_other`];
    if (other !== undefined) return other;
  }
  return translations[key] ?? key;
}

export function useTranslation() {
  const locale = useLanguageStore((s) => s.locale);
  const translations = getTranslations(locale);

  const t = useCallback(
    (key: TranslationKey, values?: InterpolationValues): string => {
      let text = resolveText(translations, locale, key, values);
      if (values) {
        for (const [k, v] of Object.entries(values)) {
          text = text.replaceAll(`{{${k}}}`, String(v));
        }
      }
      return text;
    },
    [translations, locale],
  );

  return { t, locale };
}
