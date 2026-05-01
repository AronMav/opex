"use client";

import { memo } from "react";
import { sanitizeUrl } from "@/lib/sanitize-url";
import { AudioPlayer } from "./AudioPlayer";

export const FileDataPartView = memo(function FileDataPartView({ data }: { data: { url: string; mediaType: string } }) {
  const { url, mediaType } = data;
  const safeUrl = sanitizeUrl(url);
  if (mediaType.startsWith("image/")) {
    return (
      <a href={safeUrl} target="_blank" rel="noopener noreferrer">
        <img src={safeUrl} alt="" className="max-w-md rounded-xl border border-border" loading="lazy" />
      </a>
    );
  }
  if (mediaType.startsWith("audio/")) {
    return <AudioPlayer src={safeUrl} />;
  }
  if (mediaType.startsWith("video/")) {
    return <video controls src={safeUrl} className="max-w-md rounded-xl border border-border" />;
  }
  return (
    <a href={safeUrl} target="_blank" rel="noopener noreferrer" className="text-sm text-primary underline">
      {mediaType} file
    </a>
  );
});
