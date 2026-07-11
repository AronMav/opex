import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import React from "react";

import {
  MediaNotificationBody,
  TtsNotificationBody,
  getNotificationRoute,
} from "@/components/notification-bell";

// `MediaNotificationBody` renders the inline body of a media-flavoured
// notification (TTS / image / video / generic). Voice keeps the legacy
// `tts_*` event types for UI back-compat; other kinds use kind-specific
// events emitted by `media_background.rs`.
describe("MediaNotificationBody", () => {
  // ── Voice (legacy tts_* events) ─────────────────────────────────────────────

  it("renders an audio player for tts_ready with url", () => {
    render(
      <MediaNotificationBody
        notification={{
          id: "1",
          type: "tts_ready",
          title: "Аудио готово",
          body: "Подготовлено агентом Arty",
          data: { url: "/uploads/test.wav", mediaType: "audio/wav" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />,
    );
    const audio = screen.getByTestId("tts-audio-player");
    expect(audio).toBeTruthy();
    expect(audio.getAttribute("src")).toBe("/uploads/test.wav");
  });

  it("renders error text for tts_error", () => {
    render(
      <MediaNotificationBody
        notification={{
          id: "2",
          type: "tts_error",
          title: "Не удалось синтезировать аудио",
          body: "Ошибка агента Arty: connection refused",
          data: { error: "connection refused" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />,
    );
    expect(screen.getByText(/connection refused/)).toBeTruthy();
  });

  // ── Image ───────────────────────────────────────────────────────────────────

  it("renders an inline image preview for image_ready with url", () => {
    render(
      <MediaNotificationBody
        notification={{
          id: "3",
          type: "image_ready",
          title: "Изображение готово",
          body: "Подготовлено агентом Arty",
          data: { url: "/uploads/test.png", mediaType: "image/png" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />,
    );
    const img = screen.getByTestId("image-preview") as HTMLImageElement;
    expect(img).toBeTruthy();
    expect(img.getAttribute("src")).toBe("/uploads/test.png");
    // Accessibility: the preview must carry an alt attribute (even if empty
    // is acceptable, here we expect a meaningful alt from the title).
    expect(img.getAttribute("alt")).toBeTruthy();
  });

  it("renders error text for image_error", () => {
    render(
      <MediaNotificationBody
        notification={{
          id: "4",
          type: "image_error",
          title: "Не удалось сгенерировать изображение",
          body: "Ошибка агента Arty: provider 503",
          data: { error: "provider 503" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />,
    );
    expect(screen.getByText(/provider 503/)).toBeTruthy();
  });

  // ── Video ───────────────────────────────────────────────────────────────────

  it("renders an inline video player for video_ready with url", () => {
    render(
      <MediaNotificationBody
        notification={{
          id: "5",
          type: "video_ready",
          title: "Видео готово",
          body: "Подготовлено агентом Arty",
          data: { url: "/uploads/test.mp4", mediaType: "video/mp4" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />,
    );
    const video = screen.getByTestId("video-player") as HTMLVideoElement;
    expect(video).toBeTruthy();
    expect(video.getAttribute("src")).toBe("/uploads/test.mp4");
  });

  it("renders error text for video_error", () => {
    render(
      <MediaNotificationBody
        notification={{
          id: "6",
          type: "video_error",
          title: "Не удалось сгенерировать видео",
          body: "Ошибка агента Arty: timeout",
          data: { error: "timeout" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />,
    );
    expect(screen.getByText(/timeout/)).toBeTruthy();
  });

  // ── Generic media fallback ──────────────────────────────────────────────────

  it("renders a download link for media_ready with url", () => {
    render(
      <MediaNotificationBody
        notification={{
          id: "7",
          type: "media_ready",
          title: "Медиа готово",
          body: "Подготовлено агентом Arty",
          data: { url: "/uploads/blob.bin", mediaType: "application/octet-stream" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />,
    );
    const link = screen.getByTestId("media-download") as HTMLAnchorElement;
    expect(link).toBeTruthy();
    expect(link.getAttribute("href")).toBe("/uploads/blob.bin");
  });

  it("renders error text for media_error", () => {
    render(
      <MediaNotificationBody
        notification={{
          id: "8",
          type: "media_error",
          title: "Не удалось подготовить медиа",
          body: "Ошибка агента Arty: disk full",
          data: { error: "disk full" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />,
    );
    expect(screen.getByText(/disk full/)).toBeTruthy();
  });

  // ── Default fallback for non-media types ────────────────────────────────────

  it("renders plain body text for unrelated notification types", () => {
    render(
      <MediaNotificationBody
        notification={{
          id: "9",
          type: "agent_error",
          title: "Agent failed",
          body: "Worker crashed in pipeline",
          data: {},
          read: false,
          created_at: new Date().toISOString(),
        }}
      />,
    );
    expect(screen.getByText(/Worker crashed/)).toBeTruthy();
  });

  // ── Missing url graceful handling (parametrised over media kinds) ──────────

  it.each([
    ["image_ready", "image-preview"],
    ["video_ready", "video-player"],
    ["media_ready", "media-download"],
    ["tts_ready",   "tts-audio-player"],
  ])(
    "renders body text when %s arrives without a url and skips %s",
    (eventType, testId) => {
      render(
        <MediaNotificationBody
          notification={{
            id: `nourl-${eventType}`,
            type: eventType,
            title: "Готово",
            body: "Подготовлено агентом Arty",
            data: {},
            read: false,
            created_at: new Date().toISOString(),
          }}
        />,
      );
      expect(screen.queryByTestId(testId)).toBeNull();
      expect(screen.getByText(/Подготовлено/)).toBeTruthy();
    },
  );

  // ── TtsNotificationBody back-compat alias smoke ────────────────────────────

  it("TtsNotificationBody alias renders the same audio player as MediaNotificationBody", () => {
    render(
      <TtsNotificationBody
        notification={{
          id: "alias-1",
          type: "tts_ready",
          title: "Аудио готово",
          body: "alias path",
          data: { url: "/uploads/alias.wav", mediaType: "audio/wav" },
          read: false,
          created_at: new Date().toISOString(),
        }}
      />,
    );
    const audio = screen.getByTestId("tts-audio-player");
    expect(audio.getAttribute("src")).toBe("/uploads/alias.wav");
  });
});

// ── getNotificationRoute ─────────────────────────────────────────────────────

describe("getNotificationRoute", () => {
  it.each([
    ["tts_ready"],
    ["tts_error"],
    ["image_ready"],
    ["image_error"],
    ["video_ready"],
    ["video_error"],
    ["media_ready"],
    ["media_error"],
  ])("returns null for media event %s (rendered inline, no nav)", (type) => {
    expect(getNotificationRoute(type)).toBeNull();
  });

  it.each([
    ["access_request", "/access"],
    ["tool_approval", "/monitor/?tab=approvals"],
    ["agent_error", "/monitor/?tab=logs"],
    ["watchdog_alert", "/monitor/?tab=watchdog"],
  ])("routes %s → %s", (type, expected) => {
    expect(getNotificationRoute(type)).toBe(expected);
  });

  it("falls back to /monitor/ for unknown notification types", () => {
    expect(getNotificationRoute("unknown_event")).toBe("/monitor/");
  });

  it("routes initiative_proposal to the agent's plan page using data.agent", () => {
    expect(getNotificationRoute("initiative_proposal", { agent: "Alma" })).toBe("/agents/plan/?agent=Alma");
  });

  it("falls back to /monitor/ for initiative_proposal with no agent in data", () => {
    expect(getNotificationRoute("initiative_proposal")).toBe("/monitor/");
    expect(getNotificationRoute("initiative_proposal", {})).toBe("/monitor/");
  });
});
