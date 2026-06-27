import { render, screen } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { describe, it, expect, beforeEach } from "vitest";
import { useChatStore } from "@/stores/chat-store";
import { VideoProgressIndicator } from "@/components/chat/VideoProgressIndicator";

describe("VideoProgressIndicator", () => {
  beforeEach(() => useChatStore.setState({ videoProgress: {} } as never));
  it("renders text for an active session, nothing otherwise", () => {
    const { rerender } = render(<VideoProgressIndicator sessionId="s1" />);
    expect(screen.queryByText(/Сохраняю/)).toBeNull();
    useChatStore.getState().setVideoProgress("s1", "saving", "💾 Сохраняю в Obsidian…");
    rerender(<VideoProgressIndicator sessionId="s1" />);
    expect(screen.getByText(/Сохраняю/)).toBeInTheDocument();
  });
});
