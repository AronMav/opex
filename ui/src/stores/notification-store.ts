import { create } from "zustand";
import { devtools } from "zustand/middleware";
import type { NotificationRow } from "@/types/api";

interface NotificationState {
  notifications: NotificationRow[];
  unread_count: number;
  /** Bumped only on a genuine (non-duplicate) live arrival. Drives sound/flash
   *  so refetch-on-reconnect and cold load never trigger the beep. */
  newArrivalSeq: number;
  setNotifications: (rows: NotificationRow[], count: number) => void;
  prependNotification: (row: NotificationRow) => void;
  markRead: (id: string) => void;
  markAllRead: () => void;
  clearAll: () => void;
  // Cross-tab / server-authoritative reconciliation (from WS events):
  applyRead: (id: string, unread_count: number) => void;
  applyReadAll: (unread_count: number) => void;
  applyCleared: () => void;
  resolveApproval: (approvalId: string) => void;
}

export const useNotificationStore = create<NotificationState>()(
  devtools(
    (set) => ({
      notifications: [],
      unread_count: 0,
      newArrivalSeq: 0,

      setNotifications: (rows, count) =>
        set({ notifications: rows, unread_count: count }, false, "setNotifications"),

      prependNotification: (row) =>
        set(
          (s) => {
            if (s.notifications.some((n) => n.id === row.id)) return s;
            return {
              notifications: [row, ...s.notifications],
              unread_count: s.unread_count + 1,
              newArrivalSeq: s.newArrivalSeq + 1,
            };
          },
          false,
          "prependNotification",
        ),

      markRead: (id) =>
        set(
          (s) => {
            const wasUnread = s.notifications.some((n) => n.id === id && !n.read);
            return {
              notifications: s.notifications.map((n) =>
                n.id === id ? { ...n, read: true } : n,
              ),
              unread_count: wasUnread
                ? Math.max(0, s.unread_count - 1)
                : s.unread_count,
            };
          },
          false,
          "markRead",
        ),

      markAllRead: () =>
        set(
          (s) => ({
            notifications: s.notifications.map((n) => ({ ...n, read: true })),
            unread_count: 0,
          }),
          false,
          "markAllRead",
        ),

      clearAll: () =>
        set({ notifications: [], unread_count: 0 }, false, "clearAll"),

      applyRead: (id, unread_count) =>
        set(
          (s) => ({
            notifications: s.notifications.map((n) =>
              n.id === id ? { ...n, read: true } : n,
            ),
            unread_count,
          }),
          false,
          "applyRead",
        ),

      applyReadAll: (unread_count) =>
        set(
          (s) => ({
            notifications: s.notifications.map((n) => ({ ...n, read: true })),
            unread_count,
          }),
          false,
          "applyReadAll",
        ),

      applyCleared: () =>
        set({ notifications: [], unread_count: 0 }, false, "applyCleared"),

      resolveApproval: (approvalId) =>
        set(
          (s) => {
            const target = s.notifications.find(
              (n) =>
                !n.read &&
                n.data.approval_id === approvalId,
            );
            if (!target) return s;
            return {
              notifications: s.notifications.map((n) =>
                n.id === target.id ? { ...n, read: true } : n,
              ),
              unread_count: Math.max(0, s.unread_count - 1),
            };
          },
          false,
          "resolveApproval",
        ),
    }),
    { name: "NotificationStore", enabled: process.env.NODE_ENV !== "production" },
  ),
);
