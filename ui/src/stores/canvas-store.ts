import { create } from "zustand";
import { devtools } from "zustand/middleware";
import { immer } from "zustand/middleware/immer";
import { apiDelete } from "@/lib/api";

export type CanvasContentType = "html" | "markdown" | "url" | "json";

export interface AgentCanvas {
  contentType: CanvasContentType | null;
  content: string | null;
  title: string | null;
}

interface CanvasStoreState {
  /** Per-agent canvas content */
  canvases: Record<string, AgentCanvas>;
  /** Whether the canvas split panel is open */
  panelOpen: boolean;
}

interface CanvasStoreActions {
  handleEvent(event: {
    action: string;
    agent?: string;
    content_type?: string;
    content?: string;
    title?: string | null;
  }, key?: string): void;
  clearCanvas(key: string): void;
  togglePanel(): void;
  setPanelOpen(open: boolean): void;
}

const EMPTY_CANVAS: AgentCanvas = { contentType: null, content: null, title: null };

export const useCanvasStore = create<CanvasStoreState & CanvasStoreActions>()(
  devtools(
    immer((set, get) => ({
      canvases: {},
      panelOpen: false,

      handleEvent(event, key?: string) {
        const canvasKey = key ?? event.agent;
        if (!canvasKey) return;
        set((s) => {
          switch (event.action) {
            case "present":
              s.canvases[canvasKey] = {
                contentType: (event.content_type as CanvasContentType) ?? "markdown",
                content: event.content ?? "",
                title: event.title ?? null,
              };
              s.panelOpen = true;
              break;
            case "push_data":
              s.canvases[canvasKey] = {
                contentType: "json",
                content: event.content ?? "{}",
                title: event.title ?? null,
              };
              s.panelOpen = true;
              break;
            case "clear":
              delete s.canvases[canvasKey];
              break;
          }
        });
      },

      clearCanvas(key: string) {
        set((s) => {
          delete s.canvases[key];
        });
        // Delete using agent name for backend API compatibility
        apiDelete(`/api/canvas/${key}`).catch((e) => console.error("[canvas] delete failed:", e));
      },

      togglePanel() {
        set((s) => { s.panelOpen = !s.panelOpen; });
      },

      setPanelOpen(open: boolean) {
        set((s) => { s.panelOpen = open; });
      },
    })),
    { name: "CanvasStore", enabled: process.env.NODE_ENV !== "production" },
  ),
);
