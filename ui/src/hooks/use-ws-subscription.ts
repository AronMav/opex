"use client";

import { useEffect, useRef } from "react";
import { useWsStore } from "@/stores/ws-store";
import type { WsEventType, WsEventOf } from "@/types/ws";

export function useWsSubscription<T extends WsEventType>(
  type: T,
  handler: (msg: WsEventOf<T>) => void,
) {
  const ws = useWsStore((s) => s.ws);
  const handlerRef = useRef(handler);
  useEffect(() => {
    handlerRef.current = handler;
  });

  useEffect(() => {
    if (!ws) return;
    const stableHandler = (msg: WsEventOf<T>) => handlerRef.current(msg);
    ws.on(type, stableHandler);
    return () => {
      ws.off(type, stableHandler);
    };
  }, [ws, type]);
}
