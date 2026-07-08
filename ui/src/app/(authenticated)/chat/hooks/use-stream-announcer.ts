"use client";

import { useCallback, useEffect, useRef, useState } from "react";
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

  // Mirror the latest text into a ref (in an effect, never during render) so the
  // throttled/flush callbacks read the freshest value without a stale closure.
  // Declared first so it commits before the scheduling effects below run.
  useEffect(() => {
    textRef.current = text;
  }, [text]);

  // Schedule a single trailing-edge emission of the next completed sentences.
  const scheduleEmit = useCallback(() => {
    if (timerRef.current) clearTimeout(timerRef.current);
    if (!textRef.current) return;
    timerRef.current = setTimeout(() => {
      const { toAnnounce, newOffset } = nextSentences(textRef.current, offsetRef.current);
      if (toAnnounce) {
        offsetRef.current = newOffset;
        setDelta(toAnnounce);
      }
    }, THROTTLE_MS);
  }, []);

  // New streaming message → reset and (re)schedule, so a same-text new turn still
  // announces (the [text] effect alone would not fire when text is unchanged).
  useEffect(() => {
    offsetRef.current = 0;
    setDelta("");
    scheduleEmit();
  }, [id, scheduleEmit]);

  // Throttled emission of completed sentences during streaming.
  useEffect(() => {
    scheduleEmit();
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [text, scheduleEmit]);

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
