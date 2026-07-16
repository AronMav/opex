import { describe, it, expect } from "vitest";
import fs from "node:fs";
import path from "node:path";
import type { WsEvent, WsEventType } from "@/types/ws";

const FIXTURES = path.join(__dirname, "fixtures/ws");
const EXPECTED_COUNT = 17; // one-to-one with WsEvent variants — bump when adding a variant

describe("WS wire fixtures (Rust serde <-> ts-rs)", () => {
  const files = fs.readdirSync(FIXTURES).filter((f) => f.endsWith(".json"));

  it(`covers all ${EXPECTED_COUNT} variants`, () => {
    expect(files.length).toBe(EXPECTED_COUNT);
  });

  it("no trailing newline in fixtures", () => {
    for (const f of files) {
      const raw = fs.readFileSync(path.join(FIXTURES, f));
      expect(raw[raw.length - 1]).not.toBe(0x0a);
    }
  });

  it.each(files)("%s parses into the WsEvent union", (f) => {
    const ev = JSON.parse(fs.readFileSync(path.join(FIXTURES, f), "utf8")) as WsEvent;
    const t: WsEventType = ev.type; // compile-time: type must be in the union
    expect(typeof t).toBe("string");
  });
});
