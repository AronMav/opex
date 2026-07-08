// ── CheckpointPanel.test.tsx ──────────────────────────────────────────────────
// TDD: тест написан до реализации. Покрывает:
//   - рендер списка чекпойнтов
//   - состояние «отключено» (enabled:false)
//   - состояние «пусто» (items:[])
//   - restore-confirm flow (Откатить → ConfirmDialog → mutate)

import React from "react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

// ── Мок Sheet (Radix Portal не работает в jsdom) ─────────────────────────────

vi.mock("@/components/ui/sheet", () => ({
  Sheet: ({ children, open }: { children: React.ReactNode; open?: boolean }) =>
    open ? <div data-testid="sheet-root">{children}</div> : null,
  SheetContent: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="sheet-content">{children}</div>
  ),
  SheetHeader: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="sheet-header">{children}</div>
  ),
  SheetBody: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="sheet-body">{children}</div>
  ),
  SheetTitle: ({ children }: { children: React.ReactNode }) => (
    <h2 data-testid="sheet-title">{children}</h2>
  ),
}));

// ── Мок ConfirmDialog ─────────────────────────────────────────────────────────

vi.mock("@/components/ui/confirm-dialog", () => ({
  ConfirmDialog: ({
    open,
    onConfirm,
    onClose,
    description,
    title,
  }: {
    open: boolean;
    onConfirm: () => void;
    onClose: () => void;
    description: string;
    title: string;
  }) =>
    open ? (
      <div data-testid="confirm-dialog">
        <span data-testid="confirm-title">{title}</span>
        <span data-testid="confirm-description">{description}</span>
        <button data-testid="confirm-ok" onClick={onConfirm}>
          OK
        </button>
        <button data-testid="confirm-cancel" onClick={onClose}>
          Отмена
        </button>
      </div>
    ) : null,
}));

// ── Мок Dialog ────────────────────────────────────────────────────────────────

vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({
    children,
    open,
  }: {
    children: React.ReactNode;
    open?: boolean;
  }) => (open ? <div data-testid="diff-dialog">{children}</div> : null),
  DialogContent: ({ children }: { children: React.ReactNode }) => (
    <div data-testid="diff-dialog-content">{children}</div>
  ),
  DialogHeader: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogTitle: ({ children }: { children: React.ReactNode }) => (
    <h3>{children}</h3>
  ),
}));

// ── Мок useCheckpoints / useRestoreCheckpoint ─────────────────────────────────

const mockMutate = vi.fn();
const mockUseCheckpoints = vi.fn();
const mockUseRestoreCheckpoint = vi.fn(() => ({
  mutate: mockMutate,
  isPending: false,
}));

vi.mock("@/lib/queries", () => ({
  useCheckpoints: (...args: unknown[]) => mockUseCheckpoints(...args),
  useRestoreCheckpoint: () => mockUseRestoreCheckpoint(),
}));

// ── Мок diffCheckpoint ────────────────────────────────────────────────────────

vi.mock("@/lib/api", () => ({
  diffCheckpoint: vi.fn().mockResolvedValue({ diff: "--- a/SOUL.md\n+++ b/SOUL.md\n@@ -1 +1 @@\n-old\n+new" }),
}));

// ── Мок sonner ────────────────────────────────────────────────────────────────

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn() },
}));

// ── Мок relativeTime ──────────────────────────────────────────────────────────

vi.mock("@/lib/format", () => ({
  relativeTime: (_ts: unknown) => "2h",
}));

// ── Тестовые данные ───────────────────────────────────────────────────────────

const ITEMS = [
  { n: 2, commit: "abc123", created: "2026-06-24T10:00:00Z", summary: "1 file changed" },
  { n: 1, commit: "def456", created: "2026-06-23T08:00:00Z", summary: "init checkpoint" },
];

// ── Импорт тестируемого компонента ────────────────────────────────────────────

import { CheckpointPanel } from "../CheckpointPanel";
import { useLanguageStore } from "@/stores/language-store";

// This suite asserts Russian UI strings, so pin the locale to ru regardless of
// the app's default (now English) — the real useTranslation reads the store.
beforeEach(() => useLanguageStore.setState({ locale: "ru" }));

// ── Тесты ─────────────────────────────────────────────────────────────────────

describe("CheckpointPanel — состояния", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockUseRestoreCheckpoint.mockReturnValue({ mutate: mockMutate, isPending: false });
  });

  it("рендерит список чекпойнтов", () => {
    mockUseCheckpoints.mockReturnValue({
      data: { enabled: true, items: ITEMS },
      isLoading: false,
    });

    render(<CheckpointPanel agent="Agent" open onOpenChange={() => {}} />);

    expect(screen.getByText(/1 file changed/)).toBeInTheDocument();
    expect(screen.getByText(/init checkpoint/)).toBeInTheDocument();
    expect(screen.getByText(/#2/)).toBeInTheDocument();
    expect(screen.getByText(/#1/)).toBeInTheDocument();
  });

  it("показывает «Чекпойнты отключены» когда enabled:false", () => {
    mockUseCheckpoints.mockReturnValue({
      data: { enabled: false, items: [] },
      isLoading: false,
    });

    render(<CheckpointPanel agent="Agent" open onOpenChange={() => {}} />);

    expect(screen.getByText(/Чекпойнты отключены/)).toBeInTheDocument();
  });

  it("показывает «Чекпойнтов нет» когда items пуст", () => {
    mockUseCheckpoints.mockReturnValue({
      data: { enabled: true, items: [] },
      isLoading: false,
    });

    render(<CheckpointPanel agent="Agent" open onOpenChange={() => {}} />);

    expect(screen.getByText(/Чекпойнтов нет/)).toBeInTheDocument();
  });

  it("не рендерит содержимое когда open=false", () => {
    mockUseCheckpoints.mockReturnValue({
      data: { enabled: true, items: ITEMS },
      isLoading: false,
    });

    render(<CheckpointPanel agent="Agent" open={false} onOpenChange={() => {}} />);

    expect(screen.queryByText(/1 file changed/)).not.toBeInTheDocument();
  });
});

describe("CheckpointPanel — restore confirm flow", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockUseRestoreCheckpoint.mockReturnValue({ mutate: mockMutate, isPending: false });
    mockUseCheckpoints.mockReturnValue({
      data: { enabled: true, items: ITEMS },
      isLoading: false,
    });
  });

  it("Откатить открывает ConfirmDialog с описанием", async () => {
    render(<CheckpointPanel agent="Agent" open onOpenChange={() => {}} />);

    const buttons = screen.getAllByText("Откатить");
    fireEvent.click(buttons[0]);

    await waitFor(() => {
      expect(screen.getByTestId("confirm-dialog")).toBeInTheDocument();
    });
    expect(screen.getByTestId("confirm-description").textContent).toMatch(/чекпойнту.*2/i);
  });

  it("confirm вызывает mutate с {agent, n}", async () => {
    render(<CheckpointPanel agent="Agent" open onOpenChange={() => {}} />);

    const buttons = screen.getAllByText("Откатить");
    fireEvent.click(buttons[0]); // первый = #2

    await waitFor(() => {
      expect(screen.getByTestId("confirm-dialog")).toBeInTheDocument();
    });

    fireEvent.click(screen.getByTestId("confirm-ok"));

    expect(mockMutate).toHaveBeenCalledWith(
      { agent: "Agent", n: 2 },
      expect.objectContaining({ onSuccess: expect.any(Function) }),
    );
  });

  it("cancel не вызывает mutate", async () => {
    render(<CheckpointPanel agent="Agent" open onOpenChange={() => {}} />);

    const buttons = screen.getAllByText("Откатить");
    fireEvent.click(buttons[0]);

    await waitFor(() => {
      expect(screen.getByTestId("confirm-dialog")).toBeInTheDocument();
    });

    fireEvent.click(screen.getByTestId("confirm-cancel"));

    expect(mockMutate).not.toHaveBeenCalled();
  });
});
