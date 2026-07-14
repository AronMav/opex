import { describe, it, expect, beforeEach } from "vitest";
import { useNotificationStore } from "./notification-store";
import type { NotificationRow } from "@/types/api";

function row(id: string, extra: Partial<NotificationRow> = {}): NotificationRow {
  return {
    id,
    type: "agent_error",
    title: "t",
    body: "b",
    data: {},
    read: false,
    created_at: "2026-07-14T00:00:00Z",
    ...extra,
  };
}

beforeEach(() => {
  useNotificationStore.setState({
    notifications: [],
    unread_count: 0,
    newArrivalSeq: 0,
  });
});

describe("notification-store", () => {
  it("prepend bumps unread_count and newArrivalSeq", () => {
    useNotificationStore.getState().prependNotification(row("a"));
    const s = useNotificationStore.getState();
    expect(s.unread_count).toBe(1);
    expect(s.newArrivalSeq).toBe(1);
    expect(s.notifications).toHaveLength(1);
  });

  it("duplicate prepend does not bump seq or count", () => {
    const st = useNotificationStore.getState();
    st.prependNotification(row("a"));
    st.prependNotification(row("a"));
    const s = useNotificationStore.getState();
    expect(s.unread_count).toBe(1);
    expect(s.newArrivalSeq).toBe(1);
    expect(s.notifications).toHaveLength(1);
  });

  it("markRead decrements once for an unread row", () => {
    const st = useNotificationStore.getState();
    st.prependNotification(row("a"));
    st.markRead("a");
    expect(useNotificationStore.getState().unread_count).toBe(0);
  });

  it("markRead does NOT decrement an already-read row", () => {
    useNotificationStore.setState({
      notifications: [row("a", { read: true })],
      unread_count: 0,
      newArrivalSeq: 0,
    });
    useNotificationStore.getState().markRead("a");
    expect(useNotificationStore.getState().unread_count).toBe(0);
  });

  it("applyRead sets read + server unread_count", () => {
    const st = useNotificationStore.getState();
    st.prependNotification(row("a"));
    st.prependNotification(row("b"));
    st.applyRead("a", 1);
    const s = useNotificationStore.getState();
    expect(s.notifications.find((n) => n.id === "a")?.read).toBe(true);
    expect(s.unread_count).toBe(1);
  });

  it("applyReadAll marks all read + sets count", () => {
    const st = useNotificationStore.getState();
    st.prependNotification(row("a"));
    st.prependNotification(row("b"));
    st.applyReadAll(0);
    const s = useNotificationStore.getState();
    expect(s.notifications.every((n) => n.read)).toBe(true);
    expect(s.unread_count).toBe(0);
  });

  it("applyCleared empties the list", () => {
    useNotificationStore.getState().prependNotification(row("a"));
    useNotificationStore.getState().applyCleared();
    const s = useNotificationStore.getState();
    expect(s.notifications).toHaveLength(0);
    expect(s.unread_count).toBe(0);
  });

  it("resolveApproval marks the matching unread approval row read", () => {
    useNotificationStore.setState({
      notifications: [
        row("n1", { type: "tool_approval", data: { approval_id: "ap-1" } }),
      ],
      unread_count: 1,
      newArrivalSeq: 0,
    });
    useNotificationStore.getState().resolveApproval("ap-1");
    const s = useNotificationStore.getState();
    expect(s.notifications[0].read).toBe(true);
    expect(s.unread_count).toBe(0);
  });
});
