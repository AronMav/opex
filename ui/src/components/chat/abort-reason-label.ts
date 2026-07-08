import type { TranslationKey } from "@/i18n/types";

type TFn = (key: TranslationKey, values?: Record<string, string | number>) => string;

/**
 * Map a raw `abortReason` (as emitted by the engine) to a localised label via the
 * caller-supplied `t`. Timeout variants collapse to a single key; unknown reasons
 * are surfaced verbatim through the `{{reason}}` interpolation.
 */
export function abortReasonLabel(reason: string | null | undefined, t: TFn): string {
  switch (reason) {
    case "max_duration":
      return t("chat.abort_reason_max_duration");
    case "inactivity":
      return t("chat.abort_reason_inactivity");
    case "user_cancelled":
      return t("chat.abort_reason_user_cancelled");
    case "shutdown_drain":
      return t("chat.abort_reason_shutdown_drain");
    case "connect_timeout":
    case "request_timeout":
      return t("chat.abort_reason_timeout");
    default:
      return reason
        ? t("chat.abort_reason_unknown", { reason })
        : t("chat.abort_reason_default");
  }
}
