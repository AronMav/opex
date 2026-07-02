import { test, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";

const login = vi.fn();
const replace = vi.fn();

// auth-store: page calls useAuthStore((s) => s.login) — honour the selector.
vi.mock("@/stores/auth-store", () => ({
  useAuthStore: (selector: (s: { login: typeof login }) => unknown) =>
    selector({ login }),
}));

// use-translation: identity so keys render verbatim.
vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({ t: (k: string) => k }),
}));

// next/navigation: stub router.replace.
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace }),
}));

import LoginPage from "../page";

test("renders brand, token input, and submit button", () => {
  render(<LoginPage />);
  expect(screen.getByText("OPEX")).toBeInTheDocument();
  expect(screen.getByPlaceholderText("login.enter_token")).toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "login.submit" }),
  ).toBeInTheDocument();
});
