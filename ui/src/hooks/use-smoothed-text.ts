"use client";

import { useRef, useState, useEffect } from "react";

/**
 * CharacterInterpolator — обеспечивает плавный вывод текста.
 * Адаптирует скорость печати под размер буфера (чем больше текста накопилось, тем быстрее вывод).
 *
 * Батчит обновления каждые ~40мс (25Hz) вместо 60Hz — снижает частоту вызовов
 * `marked.lexer()` в Markdown-компоненте во время стриминга в 2.5 раза без
 * заметной потери плавности анимации.
 */
const BATCH_INTERVAL_MS = 40;
// Jump ratio адаптирован под 25Hz: при 60Hz/15% очередь съедалась ~60% за 100мс,
// при 25Hz/30% съедается ~75% за 100мс — сохраняется то же ощущение отзывчивости.
const QUEUE_JUMP_RATIO = 0.3;

export function useSmoothedText(rawText: string, isStreaming: boolean) {
  const [displayedText, setDisplayValue] = useState(rawText);
  const queueRef = useRef("");
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const streamingRef = useRef(isStreaming);
  useEffect(() => {
    streamingRef.current = isStreaming;
  });

  // Синхронизируем очередь при получении новых данных
  useEffect(() => {
    if (!isStreaming) {
      setDisplayValue(rawText);
      queueRef.current = "";
      return;
    }

    // Если rawText стал короче (регенерация), сбрасываем всё
    if (rawText.length < displayedText.length) {
      setDisplayValue(rawText);
      queueRef.current = "";
      return;
    }

    queueRef.current = rawText.slice(displayedText.length);
  }, [rawText, isStreaming, displayedText.length]);

  useEffect(() => {
    if (!isStreaming && queueRef.current.length === 0) return;

    const tick = () => {
      if (queueRef.current.length > 0) {
        const jump = Math.ceil(queueRef.current.length * QUEUE_JUMP_RATIO);
        let charsToShow = Math.min(queueRef.current.length, jump);

        // C1 fix: do NOT slice between a UTF-16 surrogate pair — slicing a
        // high surrogate (0xD800–0xDBFF) apart from its low surrogate
        // (0xDC00–0xDFFF) produces a dangling surrogate that renders as
        // "\uFFFD" (the replacement char) and flickers during the stream.
        // If the cut lands between a pair, advance by one code unit so the
        // pair stays intact. The leftover low surrogate is preserved in the
        // queue for the next tick (where it rejoins its high surrogate
        // because we look at the new boundary again).
        const lastUnit = queueRef.current.charCodeAt(charsToShow - 1);
        if (lastUnit >= 0xd800 && lastUnit <= 0xdbff && charsToShow < queueRef.current.length) {
          const nextUnit = queueRef.current.charCodeAt(charsToShow);
          if (nextUnit >= 0xdc00 && nextUnit <= 0xdfff) {
            charsToShow += 1;
          }
        }

        const nextPart = queueRef.current.slice(0, charsToShow);
        queueRef.current = queueRef.current.slice(charsToShow);

        setDisplayValue((prev) => prev + nextPart);
        timerRef.current = setTimeout(tick, BATCH_INTERVAL_MS);
      } else if (streamingRef.current) {
        // Queue empty but still streaming — poll next tick
        timerRef.current = setTimeout(tick, BATCH_INTERVAL_MS);
      }
      // Otherwise: queue empty + not streaming → stop loop
    };

    timerRef.current = setTimeout(tick, BATCH_INTERVAL_MS);
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [isStreaming]);

  return displayedText;
}
