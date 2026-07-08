import { describe, it, expect } from "vitest";
import { abortReasonLabel } from "../abort-reason-label";

// M5: the label is now i18n-driven — it returns a translation key resolved by the
// caller's `t`, not a hardcoded English string.
const t = ((key: string, params?: Record<string, unknown>) =>
  params ? `${key}:${JSON.stringify(params)}` : key) as never;

describe("abortReasonLabel", () => {
  const cases: Array<[string, string]> = [
    ["max_duration", "chat.abort_reason_max_duration"],
    ["inactivity", "chat.abort_reason_inactivity"],
    ["user_cancelled", "chat.abort_reason_user_cancelled"],
    ["shutdown_drain", "chat.abort_reason_shutdown_drain"],
    ["connect_timeout", "chat.abort_reason_timeout"],
    ["request_timeout", "chat.abort_reason_timeout"],
  ];
  for (const [reason, key] of cases) {
    it(`maps ${reason} → ${key}`, () => {
      expect(abortReasonLabel(reason, t)).toBe(key);
    });
  }

  it("maps an unknown reason to the unknown key with the raw reason", () => {
    expect(abortReasonLabel("something_new", t)).toBe(
      'chat.abort_reason_unknown:{"reason":"something_new"}',
    );
  });

  it("maps null/undefined to the default key", () => {
    expect(abortReasonLabel(null, t)).toBe("chat.abort_reason_default");
    expect(abortReasonLabel(undefined, t)).toBe("chat.abort_reason_default");
  });
});
