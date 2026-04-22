import { parseSseEvent } from "./stream/sse-parser";

it("parseSseEvent handles reconnecting event", () => {
  const result = parseSseEvent(JSON.stringify({ type: "reconnecting", attempt: 2, delay_ms: 4000 }));
  expect(result).toEqual({ type: "reconnecting", attempt: 2, delay_ms: 4000 });
});

it("parseSseEvent returns null for unknown type", () => {
  const result = parseSseEvent(JSON.stringify({ type: "unknown-type" }));
  expect(result).toBeNull();
});
