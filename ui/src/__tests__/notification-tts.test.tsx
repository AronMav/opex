import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import React from "react";

// Minimal stub for the notification item renderer we're about to add
// Import will fail until we export the component
import { TtsNotificationBody } from "@/components/notification-bell";

describe("TtsNotificationBody", () => {
  it("renders an audio player for tts_ready with url", () => {
    render(
      <TtsNotificationBody
        notification={{
          id: "1",
          type: "tts_ready",
          title: "Аудио готово",
          body: "Синтезировано агентом Arty",
          data: { url: "/uploads/test.wav", mediaType: "audio/wav" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />
    );
    const audio = screen.getByTestId("tts-audio-player");
    expect(audio).toBeTruthy();
    expect(audio.getAttribute("src")).toBe("/uploads/test.wav");
  });

  it("renders error text for tts_error", () => {
    render(
      <TtsNotificationBody
        notification={{
          id: "2",
          type: "tts_error",
          title: "Не удалось синтезировать аудио",
          body: "Ошибка агента Arty: connection refused",
          data: { error: "connection refused" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />
    );
    expect(screen.getByText(/connection refused/)).toBeTruthy();
  });
});
