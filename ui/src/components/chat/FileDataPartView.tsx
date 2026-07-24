"use client";

import { memo } from "react";
import { FileText, Image as ImageIcon, Music, Video, Archive, Code, FileType, Download, ExternalLink } from "lucide-react";
import { sanitizeUrl } from "@/lib/sanitize-url";
import { AudioPlayer } from "./AudioPlayer";
import { ImageLightbox } from "./ImageLightbox";
import { cn } from "@/lib/utils";

export interface FileDataPart {
  url: string;
  mediaType: string;
  filename?: string;
}

function friendlyLabel(mediaType: string): string {
  const m = (mediaType || "").toLowerCase();
  if (m === "application/pdf") return "PDF";
  if (m === "application/json" || m === "application/x-json") return "JSON";
  if (m === "application/xml") return "XML";
  if (m.startsWith("text/csv")) return "CSV";
  if (m.startsWith("text/markdown")) return "Markdown";
  if (m.startsWith("text/html")) return "HTML";
  if (m.startsWith("text/css")) return "CSS";
  if (m.startsWith("text/javascript") || m === "application/javascript") return "JavaScript";
  if (m === "application/x-yaml" || m === "application/yaml" || m === "text/yaml") return "YAML";
  if (m === "application/x-sh") return "Shell";
  if (m.startsWith("text/")) return "Text";
  if (m === "application/zip" || m === "application/x-zip-compressed") return "ZIP";
  if (m === "application/gzip" || m === "application/x-gzip" || m.endsWith("+gzip")) return "GZIP";
  if (m === "application/x-tar" || m === "application/x-bzip2") return "Archive";
  if (m.startsWith("application/vnd.openxmlformats-officedocument")) return "Office";
  if (m.startsWith("application/vnd.oasis.opendocument")) return "Office";
  if (m === "application/msword") return "Word";
  if (m.startsWith("application/")) return "Document";
  return "File";
}

type FileFamily = "image" | "audio" | "video" | "pdf" | "code" | "archive" | "doc" | "generic";

function classifyMediaType(mediaType: string, label: string): FileFamily {
  const m = (mediaType || "").toLowerCase();
  if (m.startsWith("image/")) return "image";
  if (m.startsWith("audio/")) return "audio";
  if (m.startsWith("video/")) return "video";
  if (m === "application/pdf") return "pdf";
  if (label.includes("JSON") || label.includes("YAML") || label.includes("JavaScript") || label.includes("CSS") || label.includes("HTML") || label.includes("Shell") || label.includes("XML")) return "code";
  if (label.includes("ZIP") || label.includes("GZIP") || label.includes("Archive")) return "archive";
  if (label.includes("Office") || label.includes("Word") || label.includes("Document") || label.includes("CSV") || label.includes("Markdown") || label.includes("Text")) return "doc";
  return "generic";
}

const FAMILY_STYLES: Record<FileFamily, { icon: typeof FileText; bg: string; text: string; ring: string }> = {
  image:  { icon: ImageIcon, bg: "bg-emerald-500/12",  text: "text-emerald-500",  ring: "ring-emerald-500/20" },
  audio:  { icon: Music,     bg: "bg-purple-500/12",   text: "text-purple-500",   ring: "ring-purple-500/20" },
  video:  { icon: Video,     bg: "bg-blue-500/12",     text: "text-blue-500",     ring: "ring-blue-500/20" },
  pdf:    { icon: FileText,  bg: "bg-red-500/12",      text: "text-red-500",      ring: "ring-red-500/20" },
  code:   { icon: Code,      bg: "bg-amber-500/12",    text: "text-amber-500",    ring: "ring-amber-500/20" },
  archive:{ icon: Archive,   bg: "bg-orange-500/12",   text: "text-orange-500",   ring: "ring-orange-500/20" },
  doc:    { icon: FileType,  bg: "bg-sky-500/12",      text: "text-sky-500",      ring: "ring-sky-500/20" },
  generic:{ icon: FileText,  bg: "bg-primary/12",      text: "text-primary",      ring: "ring-primary/20" },
};

function extFromUrl(url: string): string | null {
  try {
    const u = new URL(url, window.location.origin);
    const path = u.pathname;
    const dot = path.lastIndexOf(".");
    if (dot < 0 || dot === path.length - 1) return null;
    return path.slice(dot + 1).toUpperCase();
  } catch {
    return null;
  }
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
    return (
      <div className="group relative max-w-[min(28rem,100%)] overflow-hidden rounded-xl border border-border bg-card shadow-[var(--elevation-2)] transition-shadow hover:shadow-[var(--elevation-3)]">
        <video controls src={safeUrl} className="w-full" />
      </div>
    );
  }

  const label = filename?.trim() || friendlyLabel(mediaType);
  const family = classifyMediaType(mediaType, label);
  const styles = FAMILY_STYLES[family];
  const Icon = styles.icon;
  const ext = extFromUrl(url);

  return (
    <a
      href={safeUrl}
      target="_blank"
      rel="noopener noreferrer"
      download={filename || undefined}
      className={cn(
        "group flex max-w-[min(26rem,100%)] items-center gap-3 rounded-2xl border border-border bg-card p-3 shadow-[var(--elevation-2)]",
        "transition-all duration-200 hover:border-transparent hover:shadow-[var(--elevation-4)]",
        "hover:ring-2",
        styles.ring,
      )}
      title={mediaType}
    >
      <span className={cn("flex h-10 w-10 shrink-0 items-center justify-center rounded-xl", styles.bg)}>
        <Icon className={cn("h-5 w-5", styles.text)} />
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="truncate text-sm font-medium text-foreground">
          {filename?.trim() || label}
        </span>
        <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
          {ext && <span className="font-mono font-semibold uppercase">{ext}</span>}
          {ext && <span aria-hidden>·</span>}
          <span>{label}</span>
        </span>
      </span>
      <span className="flex shrink-0 items-center gap-1">
        <span className="flex h-7 w-7 items-center justify-center rounded-lg text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100">
          <ExternalLink className="h-3.5 w-3.5" />
        </span>
        <span className="flex h-7 w-7 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted">
          <Download className="h-3.5 w-3.5" />
        </span>
      </span>
    </a>
  );
});
