import { test, expect, vi } from "vitest";
import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { ErrorBanner, classifyError } from "../error-banner";

vi.mock("@/hooks/use-translation", () => ({
  useTranslation: () => ({
    t: (key: string, params?: Record<string, unknown>) => {
      if (key === "common.error_prefix") return `Ошибка: ${params?.error}`;
      return key;
    },
    locale: "ru",
  }),
}));

// Страницы передают stringified Error (`${error}` / String(error)) — баннер не
// должен показывать технический префикс «Error:» перед сообщением.
test("strips the Error: prefix from stringified Error objects", () => {
  render(<ErrorBanner error="Error: HTTP 404" />);
  expect(screen.getByText("Ошибка: HTTP 404")).toBeInTheDocument();
});

test("collapses a doubled Error: prefix", () => {
  render(<ErrorBanner error="Error: Error: HTTP 502" />);
  expect(screen.getByText("Ошибка: HTTP 502")).toBeInTheDocument();
});

test("strips typed error prefixes (TypeError:)", () => {
  render(<ErrorBanner error="TypeError: fetch failed" />);
  expect(screen.getByText("Ошибка: fetch failed")).toBeInTheDocument();
});

test("leaves plain messages untouched", () => {
  render(<ErrorBanner error="агент не найден" />);
  expect(screen.getByText("Ошибка: агент не найден")).toBeInTheDocument();
});

test("classifyError pins: network text is connection_lost, timeout is timeout", () => {
  expect(classifyError("Failed to fetch")).toBe("connection_lost");
  expect(classifyError("Request timed out")).toBe("timeout");
  expect(classifyError("HTTP 500")).toBe("api_error");
});
