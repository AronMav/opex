"use client";

import { Button } from "@/components/ui/button";
import { useState, useCallback, useEffect, useRef } from "react";
import { X, ZoomIn, ZoomOut, Download } from "lucide-react";
import { cn } from "@/lib/utils";
import { useTranslation } from "@/hooks/use-translation";
import { useFocusTrap } from "@/hooks/use-focus-trap";

interface ImageLightboxProps {
  src: string;
  alt?: string;
  className?: string;
}

export function ImageLightbox({ src, alt = "", className }: ImageLightboxProps) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [zoom, setZoom] = useState(1);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);

  const handleOpen = useCallback(() => {
    setOpen(true);
    setZoom(1);
  }, []);

  const handleClose = useCallback(() => {
    setOpen(false);
    setZoom(1);
    // Focus is restored to the trigger by useFocusTrap when `open` flips false.
  }, []);

  // Focus into the dialog on open, restore to trigger on close, trap Tab within.
  const handleDialogKeyDown = useFocusTrap({
    active: open,
    containerRef: dialogRef,
    restoreTo: triggerRef,
    initialFocus: "container",
  });

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
      <Button
        ref={triggerRef}
        type="button"
        variant="ghost"
        size="icon"
        onClick={handleOpen}
        className="cursor-zoom-in h-auto w-auto p-0 hover:bg-transparent"
        aria-label={t("chat.lightbox_open")}
      >
        <img src={src} alt={alt} className={cn("max-w-[min(28rem,100%)] rounded-xl border border-border", className)} loading="lazy" />
      </Button>

      {open && (
        <div
          ref={dialogRef}
          tabIndex={-1}
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 backdrop-blur-sm overflow-auto outline-none"
          onClick={handleClose}
          onKeyDown={handleDialogKeyDown}
          role="dialog"
          aria-modal="true"
          aria-label={t("chat.lightbox_viewer")}
        >
          {/* Toolbar */}
          <div
            className="absolute top-[max(1rem,env(safe-area-inset-top))] right-4 flex items-center gap-2 z-10"
            onClick={(e) => e.stopPropagation()}
          >
            <Button
              type="button"
              variant="ghost"
              size="icon"
              onClick={handleZoomOut}
              className="rounded-lg bg-white/10 text-white hover:bg-white/20"
              aria-label={t("chat.lightbox_zoom_out")}
            >
              <ZoomOut className="h-4 w-4" />
            </Button>
            <span className="text-white/60 text-xs font-mono min-w-[3ch] text-center">
              {Math.round(zoom * 100)}%
            </span>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              onClick={handleZoomIn}
              className="rounded-lg bg-white/10 text-white hover:bg-white/20"
              aria-label={t("chat.lightbox_zoom_in")}
            >
              <ZoomIn className="h-4 w-4" />
            </Button>
            <a
              href={src}
              download
              className="rounded-lg bg-white/10 p-2 text-white hover:bg-white/20 transition-colors"
              aria-label={t("chat.lightbox_download")}
            >
              <Download className="h-4 w-4" />
            </a>
            <button
              type="button"
              onClick={handleClose}
              className="rounded-lg bg-white/10 p-2 text-white hover:bg-white/20 transition-colors"
              aria-label={t("common.close")}
            >
              <X className="h-4 w-4" />
            </button>
          </div>

          {/* Image */}
          <img
            src={src}
            alt={alt}
            className="max-h-[90dvh] max-w-[90dvw] object-contain transition-transform duration-200"
            style={{ transform: `scale(${zoom})` }}
            onClick={(e) => e.stopPropagation()}
          />
        </div>
      )}
    </>
  );
}
