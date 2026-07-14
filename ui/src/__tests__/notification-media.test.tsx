import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import React from "react";

import {
  MediaNotificationBody,
  getNotificationRoute,
} from "@/components/notification-bell";

// `MediaNotificationBody` renders the inline body of a media-flavoured
// notification (TTS / image / video / generic). Voice keeps the legacy
// `tts_*` event types for UI back-compat; other kinds use kind-specific
// events emitted by `media_background.rs`.
describe("MediaNotificationBody", () => {
  // ── Voice (legacy tts_* events) ─────────────────────────────────────────────

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
});

// ── getNotificationRoute ─────────────────────────────────────────────────────

describe("getNotificationRoute", () => {
  it.each([
    ["tts_error"],
    ["image_error"],
    ["video_error"],
    ["media_error"],
  ])("returns null for media-error event %s (rendered inline, no nav)", (type) => {
    expect(getNotificationRoute(type)).toBeNull();
  });

  it.each([
    ["tts_ready"],
    ["image_ready"],
    ["video_ready"],
    ["media_ready"],
  ])("routes former media-ready event %s to /monitor/ (no longer emitted)", (type) => {
    expect(getNotificationRoute(type)).toBe("/monitor/");
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
