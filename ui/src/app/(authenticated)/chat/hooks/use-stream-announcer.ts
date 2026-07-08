"use client";

import { useEffect, useRef, useState } from "react";
import { nextSentences } from "@/app/(authenticated)/chat/stream-announce";

const THROTTLE_MS = 600;

/**
 * Turns a streaming assistant message (id + growing text + streaming flag) into
 * a `delta` string of newly-completed sentences, suitable for a polite live
 * region. Resets per message id, throttles emissions during streaming, and
 * flushes the trailing fragment when streaming ends.
 */
export function useStreamAnnouncer(id: string, text: string, streaming: boolean): string {
  const [delta, setDelta] = useState("");
  const offsetRef = useRef(0);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const textRef = useRef(text);
  textRef.current = text;

  // New streaming message → announce it from the start.
  useEffect(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    offsetRef.current = 0;
    setDelta("");
    // Trigger a new timer for the reset message id.
    if (text) {
      timerRef.current = setTimeout(() => {
        const { toAnnounce, newOffset } = nextSentences(textRef.current, offsetRef.current);
        if (toAnnounce) {
          offsetRef.current = newOffset;
          setDelta(toAnnounce);
        }
      }, THROTTLE_MS);
    }
  }, [id]);

  // Throttled emission of completed sentences during streaming.
  useEffect(() => {
    if (!text) return;
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(() => {
      const { toAnnounce, newOffset } = nextSentences(textRef.current, offsetRef.current);
      if (toAnnounce) {
        offsetRef.current = newOffset;
        setDelta(toAnnounce);
      }
    }, THROTTLE_MS);
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [text]);

  // Flush the trailing fragment once streaming stops.
  useEffect(() => {
    if (streaming) return;
    if (timerRef.current) clearTimeout(timerRef.current);
    const { toAnnounce, newOffset } = nextSentences(textRef.current, offsetRef.current, { flush: true });
    if (toAnnounce) {
      offsetRef.current = newOffset;
      setDelta(toAnnounce);
    }
  }, [streaming]);

  return delta;
}
