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

  it("markRead does NOT decrement for an already-read row (guarded)", () => {
    // One already-read row + one unread row → unread_count = 1.
    // Old buggy code decremented unconditionally to 0; the fix must keep it at 1.
    useNotificationStore.setState({
      notifications: [row("a", { read: true }), row("b")],
      unread_count: 1,
      newArrivalSeq: 0,
    });
    useNotificationStore.getState().markRead("a");
    expect(useNotificationStore.getState().unread_count).toBe(1);
  });

  it("applyRead assigns the server unread_count verbatim (not local math)", () => {
    const st = useNotificationStore.getState();
    st.prependNotification(row("a"));
    st.prependNotification(row("b"));
    // Server says 5. Local decrement math (2-1=1) would give a different number,
    // so a passing assertion proves the server value is assigned verbatim.
    st.applyRead("a", 5);
    const s = useNotificationStore.getState();
    expect(s.notifications.find((n) => n.id === "a")?.read).toBe(true);
    expect(s.unread_count).toBe(5);
  });

  it("applyReadAll marks all read + assigns server count verbatim", () => {
    const st = useNotificationStore.getState();
    st.prependNotification(row("a"));
    st.prependNotification(row("b"));
    // Non-zero proves the arg is assigned, not a hardcoded 0.
    st.applyReadAll(7);
    const s = useNotificationStore.getState();
    expect(s.notifications.every((n) => n.read)).toBe(true);
    expect(s.unread_count).toBe(7);
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

  it("syncFirstPage merges: refreshes known, prepends new, preserves older, sets count, no beep", () => {
    // Seed: 3 rows loaded (b newest ... but list order is [new..old]); pretend
    // "old1"/"old2" are older pages already appended.
    useNotificationStore.setState({
      notifications: [row("n2", { read: false }), row("old1"), row("old2")],
      unread_count: 3,
      newArrivalSeq: 5,
    });
    // Server first page: a brand-new "n3", n2 now read, (old rows not in page).
    useNotificationStore
      .getState()
      .syncFirstPage([row("n3"), row("n2", { read: true })], 1);
    const s = useNotificationStore.getState();
    expect(s.notifications.map((n) => n.id)).toEqual(["n3", "n2", "old1", "old2"]);
    expect(s.notifications.find((n) => n.id === "n2")?.read).toBe(true); // refreshed
    expect(s.unread_count).toBe(1); // server value
    expect(s.newArrivalSeq).toBe(5); // unchanged — refetch never beeps
  });

  it("appendOlder dedup-appends to the tail without touching count or seq", () => {
    useNotificationStore.setState({
      notifications: [row("a"), row("b")],
      unread_count: 2,
      newArrivalSeq: 4,
    });
    // "b" is a duplicate (boundary overlap); "c","d" are genuinely older.
    useNotificationStore.getState().appendOlder([row("b"), row("c"), row("d")]);
    const s = useNotificationStore.getState();
    expect(s.notifications.map((n) => n.id)).toEqual(["a", "b", "c", "d"]);
    expect(s.unread_count).toBe(2);
    expect(s.newArrivalSeq).toBe(4);
  });
});
