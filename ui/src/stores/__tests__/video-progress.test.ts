import { describe, it, expect, beforeEach } from "vitest";
import { useChatStore } from "@/stores/chat-store";

describe("videoProgress store", () => {
  beforeEach(() => {
    useChatStore.setState({ videoProgress: {} } as never);
  });
  it("set then clear", () => {
    useChatStore.getState().setVideoProgress("s1", "fetch", "качаю");
    expect(useChatStore.getState().videoProgress["s1"]).toEqual({ phase: "fetch", text: "качаю" });
    useChatStore.getState().setVideoProgress("s1", "saving", "сохраняю");
    expect(useChatStore.getState().videoProgress["s1"].phase).toBe("saving");
    useChatStore.getState().clearVideoProgress("s1");
    expect(useChatStore.getState().videoProgress["s1"]).toBeUndefined();
  });
});
