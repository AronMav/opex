import { test, expect, vi, beforeEach } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";

const backups = [
  { filename: "opex-2026-01-01.tar.gz", size_bytes: 1024, created_at: "2026-01-01T00:00:00Z" },
  { filename: "opex-2026-01-02.tar.gz", size_bytes: 2048, created_at: "2026-01-02T00:00:00Z" },
];

vi.mock("@/lib/queries", () => ({
  useBackups: () => ({ data: backups, isLoading: false, error: null, refetch: vi.fn() }),
  useCreateBackup: () => ({ mutate: vi.fn(), isPending: false, error: null }),
}));
// /api/config resolves with no backup config → BackupSettings renders nothing.
vi.mock("@/lib/api", () => ({
  getToken: () => "tok",
  apiGet: vi.fn(() => Promise.resolve({})),
  apiPut: vi.fn(() => Promise.resolve({})),
  apiDelete: vi.fn(() => Promise.resolve({})),
}));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { success: vi.fn(), error: vi.fn(), warning: vi.fn() }) }));
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k, locale: "en" }),
}));

import BackupsPage from "../page";

beforeEach(() => vi.clearAllMocks());

test("renders the migrated backup rows (filename + size)", () => {
  render(<BackupsPage />);
  expect(screen.getByText("opex-2026-01-01.tar.gz")).toBeInTheDocument();
  expect(screen.getByText("opex-2026-01-02.tar.gz")).toBeInTheDocument();
  // formatBytes output for the first row's size is rendered
  expect(screen.getByText("1.0 KB")).toBeInTheDocument();
});

test("restore confirm dialog shows the Restore label (not Delete)", () => {
  render(<BackupsPage />);
  // Trigger the restore confirm for the first row.
  fireEvent.click(screen.getAllByTitle("backups.restore")[0]);
  // ConfirmDialog is open with the restore title...
  expect(screen.getByText("backups.restore_title")).toBeInTheDocument();
  // ...and its confirm action is labelled "Restore", NOT "Delete".
  const dialog = screen.getByRole("alertdialog");
  const confirmBtn = within(dialog).getByRole("button", { name: "backups.restore" });
  expect(confirmBtn).toHaveAttribute("data-variant", "warning");
  expect(within(dialog).queryByText("common.delete")).not.toBeInTheDocument();
});
