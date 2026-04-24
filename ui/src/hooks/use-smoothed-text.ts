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
  streamingRef.current = isStreaming;

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
        const charsToShow = Math.min(queueRef.current.length, jump);

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
