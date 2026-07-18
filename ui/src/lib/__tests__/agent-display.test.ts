import { describe, it, expect } from "vitest";
import { displayAgentName } from "@/lib/agent-display";

const t = (key: string) => key;

describe("displayAgentName (WS6)", () => {
  it("returns the generic label for a UUID-shaped agentId even if it happens to be in knownAgents", () => {
    const uuid = "550e8400-e29b-41d4-a716-446655440000";
    expect(displayAgentName(uuid, [uuid], t)).toBe("chat.unknown_agent");
  });

  it("returns the generic label when agentId is not in the known-agents list", () => {
    expect(displayAgentName("SomeGhostAgent", ["Arty", "Helper"], t)).toBe("chat.unknown_agent");
  });

  it("returns the real agent name unchanged when it is a known, non-UUID agent", () => {
    expect(displayAgentName("Arty", ["Arty", "Helper"], t)).toBe("Arty");
  });

  it("is case-insensitive when detecting UUID shape", () => {
    const uuid = "550E8400-E29B-41D4-A716-446655440000";
    expect(displayAgentName(uuid, [uuid], t)).toBe("chat.unknown_agent");
  });
});
