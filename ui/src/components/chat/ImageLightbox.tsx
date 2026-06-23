"use client";

import { useState, useCallback, useEffect } from "react";
import { X, ZoomIn, ZoomOut, Download } from "lucide-react";
import { cn } from "@/lib/utils";

interface ImageLightboxProps {
  src: string;
  alt?: string;
  className?: string;
}

export function ImageLightbox({ src, alt = "", className }: ImageLightboxProps) {
  const [open, setOpen] = useState(false);
  const [zoom, setZoom] = useState(1);

  const handleOpen = useCallback(() => {
    setOpen(true);
    setZoom(1);
  }, []);

  const handleClose = useCallback(() => {
    setOpen(false);
    setZoom(1);
  }, []);

  const handleZoomIn = useCallback(() => {
    setZoom((z) => Math.min(z + 0.5, 3));
  }, []);

  const handleZoomOut = useCallback(() => {
    setZoom((z) => Math.max(z - 0.5, 0.5));
  }, []);

  // Close on Escape
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") handleClose();
      if (e.key === "+" || e.key === "=") handleZoomIn();
      if (e.key === "-") handleZoomOut();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, handleClose, handleZoomIn, handleZoomOut]);

  // Prevent body scroll when open
  useEffect(() => {
    if (open) {
      const prev = document.body.style.overflow;
      document.body.style.overflow = "hidden";
      return () => { document.body.style.overflow = prev; };
    }
  }, [open]);

  return (
    <>
      <button
        type="button"
        onClick={handleOpen}
        className="cursor-zoom-in"
        aria-label="Open image"
      >
        <img src={src} alt={alt} className={cn("max-w-[min(28rem,100%)] rounded-xl border border-border", className)} loading="lazy" />
      </button>

      {open && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 backdrop-blur-sm overflow-auto"
          onClick={handleClose}
          role="dialog"
          aria-modal="true"
          aria-label="Image viewer"
        >
          {/* Toolbar */}
          <div
            className="absolute top-[max(1rem,env(safe-area-inset-top))] right-4 flex items-center gap-2 z-10"
            onClick={(e) => e.stopPropagation()}
          >
            <button
              type="button"
              onClick={handleZoomOut}
              className="rounded-lg bg-white/10 p-2 text-white hover:bg-white/20 transition-colors"
              aria-label="Zoom out"
            >
              <ZoomOut className="h-4 w-4" />
            </button>
            <span className="text-white/60 text-xs font-mono min-w-[3ch] text-center">
              {Math.round(zoom * 100)}%
            </span>
            <button
              type="button"
              onClick={handleZoomIn}
              className="rounded-lg bg-white/10 p-2 text-white hover:bg-white/20 transition-colors"
              aria-label="Zoom in"
            >
              <ZoomIn className="h-4 w-4" />
            </button>
            <a
              href={src}
              download
              className="rounded-lg bg-white/10 p-2 text-white hover:bg-white/20 transition-colors"
              aria-label="Download image"
            >
              <Download className="h-4 w-4" />
            </a>
            <button
              type="button"
              onClick={handleClose}
              className="rounded-lg bg-white/10 p-2 text-white hover:bg-white/20 transition-colors"
              aria-label="Close"
            >
              <X className="h-4 w-4" />
            </button>
          </div>

          {/* Image */}
          <img
            src={src}
            alt={alt}
            className="max-h-[90dvh] max-w-[90vw] object-contain transition-transform duration-200"
            style={{ transform: `scale(${zoom})` }}
            onClick={(e) => e.stopPropagation()}
          />
        </div>
      )}
    </>
  );
}
