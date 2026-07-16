import { create } from "zustand";

/** What the palette's Enter/click selection points at. Written by
 *  SearchPalette in Task 4 (jump-to-message); consumed by the chat page to
 *  scroll to + flash the target message. */
export interface PaletteTarget {
  sessionId: string;
  messageId?: string;
}

interface PaletteState {
  open: boolean;
  setOpen: (v: boolean) => void;
  target: PaletteTarget | null;
  setTarget: (t: PaletteTarget | null) => void;
  /** Message id to visually highlight once the target session/message has
   *  been navigated to (Task 3/4 consume this to flash the row). */
  highlightedMessageId: string | null;
  setHighlighted: (id: string | null) => void;
}

export const usePaletteStore = create<PaletteState>((set) => ({
  open: false,
  setOpen: (v) => set({ open: v }),
  target: null,
  setTarget: (t) => set({ target: t }),
  highlightedMessageId: null,
  setHighlighted: (id) => set({ highlightedMessageId: id }),
}));
