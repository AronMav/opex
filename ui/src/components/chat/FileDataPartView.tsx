"use client";

import { memo } from "react";
import { sanitizeUrl } from "@/lib/sanitize-url";
import { AudioPlayer } from "./AudioPlayer";
import { ImageLightbox } from "./ImageLightbox";

export const FileDataPartView = memo(function FileDataPartView({ data }: { data: { url: string; mediaType: string } }) {
  const { url, mediaType } = data;
  const safeUrl = sanitizeUrl(url);
  if (mediaType.startsWith("image/")) {
    return <ImageLightbox src={safeUrl} />;
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
