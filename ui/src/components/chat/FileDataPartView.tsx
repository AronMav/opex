"use client";

import { memo } from "react";
import { sanitizeUrl } from "@/lib/sanitize-url";
import { AudioPlayer } from "./AudioPlayer";
import { ImageLightbox } from "./ImageLightbox";

export interface FileDataPart {
  url: string;
  mediaType: string;
  /** Original filename when known (user uploads). Absent for assistant-generated
   *  artifacts — UI falls back to a MIME-based label. */
  filename?: string;
}

/**
 * Human-friendly label for a file with no filename, derived from its MIME type.
 * Avoids the bare `{mediaType} file` rendering that leaked `text/plain file`
 * etc. straight into the chat.
 *
 * Order matters — check the longest prefixes first so `application/json`
 * doesn't fall through to the catch-all `application/*` arm.
 */
function friendlyLabel(mediaType: string): string {
  const m = (mediaType || "").toLowerCase();
  if (m === "application/pdf") return "PDF document";
  if (m === "application/json" || m === "application/x-json") return "JSON file";
  if (m === "application/xml") return "XML document";
  if (m.startsWith("text/")) return "Text file";
  if (m === "application/zip" || m === "application/x-zip-compressed") return "ZIP archive";
  if (m === "application/gzip" || m === "application/x-gzip" || m.endsWith("+gzip")) return "GZIP archive";
  if (m === "application/x-tar" || m === "application/x-bzip2") return "Archive";
  if (m.startsWith("application/vnd.openxmlformats-officedocument")) return "Office document";
  if (m.startsWith("application/vnd.oasis.opendocument")) return "Office document";
  if (m === "application/msword") return "Word document";
  if (m === "application/x-yaml" || m === "application/yaml" || m === "text/yaml") return "YAML file";
  if (m === "text/csv") return "CSV table";
  if (m === "text/markdown") return "Markdown file";
  if (m === "text/html") return "HTML document";
  if (m === "text/css") return "CSS stylesheet";
  if (m === "text/javascript" || m === "application/javascript") return "JavaScript file";
  if (m === "application/x-sh") return "Shell script";
  if (m.startsWith("application/")) return "Document";
  // Last-resort fallback: don't leak the raw MIME, just call it a file.
  return "File";
}

/** Small inline glyph (emoji) by file family — matches the rendering branches
 *  below so images/audio/video keep their dedicated players. */
function iconFor(mediaType: string, label: string): string {
  const m = (mediaType || "").toLowerCase();
  if (m === "application/pdf") return "📄";
  if (m.startsWith("text/")) return "📝";
  if (label.includes("archive") || label.includes("ZIP") || label.includes("GZIP")) return "🗄️";
  if (label.includes("Office") || label.includes("Word")) return "📃";
  if (label.includes("JSON") || label.includes("YAML") || label.includes("JavaScript") || label.includes("CSS") || label.includes("HTML") || label.includes("Shell")) return "📜";
  return "📎";
}

export const FileDataPartView = memo(function FileDataPartView({ data }: { data: FileDataPart }) {
  const { url, mediaType, filename } = data;
  const safeUrl = sanitizeUrl(url);
  if (mediaType.startsWith("image/")) {
    return <ImageLightbox src={safeUrl} />;
  }
  if (mediaType.startsWith("audio/")) {
    return <AudioPlayer src={safeUrl} />;
  }
  if (mediaType.startsWith("video/")) {
    return <video controls src={safeUrl} className="max-w-[min(28rem,100%)] rounded-xl border border-border" />;
  }
  const label = filename?.trim() || friendlyLabel(mediaType);
  const icon = iconFor(mediaType, label);
  return (
    <a
      href={safeUrl}
      target="_blank"
      rel="noopener noreferrer"
      download={filename || undefined}
      className="inline-flex max-w-[min(28rem,100%)] items-center gap-2 rounded-xl border border-border bg-muted/40 px-3 py-2 text-sm text-foreground transition-colors hover:bg-muted"
      title={mediaType}
    >
      <span aria-hidden className="text-base leading-none">{icon}</span>
      <span className="truncate font-medium underline-offset-2 hover:underline">{label}</span>
    </a>
  );
});
