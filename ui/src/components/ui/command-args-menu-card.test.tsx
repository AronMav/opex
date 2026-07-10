import "@testing-library/jest-dom/vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { CommandArgsMenuCard } from "./command-args-menu-card";
import { apiPost } from "@/lib/api";

vi.mock("@/lib/api", () => ({
  apiPost: vi.fn().mockResolvedValue({}),
}));

const mockApiPost = vi.mocked(apiPost);

beforeEach(() => {
  mockApiPost.mockClear();
});

it("renders the prompt text", () => {
  render(
    <CommandArgsMenuCard
      data={{ card_type: "command_args_menu", command: "summarize_video", text: "Пришлите ссылку" }}
    />
  );
  expect(screen.getByText(/Пришлите ссылку/)).toBeInTheDocument();
});

it("renders options as buttons when present", () => {
  render(
    <CommandArgsMenuCard
      data={{
        card_type: "command_args_menu",
        command: "summarize_video",
        text: "Выберите язык",
        options: [
          { value: "ru", label: "Русский" },
          { value: "en", label: "English" },
        ],
      }}
    />
  );
  const buttons = screen.getAllByRole("button");
  expect(buttons).toHaveLength(2);
  expect(screen.getByText("Русский")).toBeInTheDocument();
  expect(screen.getByText("English")).toBeInTheDocument();
});

it("renders no buttons when options are absent", () => {
  render(
    <CommandArgsMenuCard data={{ card_type: "command_args_menu", command: "summarize_video", text: "Пришлите ссылку" }} />
  );
  expect(screen.queryAllByRole("button")).toHaveLength(0);
});

it("posts token + chosen value to /api/commands/menu-run on click", () => {
  render(
    <CommandArgsMenuCard
      data={{
        card_type: "command_args_menu",
        command: "summarize_video",
        text: "Выберите длину",
        token: "t",
        options: [
          { value: "short", label: "Коротко" },
          { value: "long", label: "Подробно" },
        ],
      }}
    />
  );
  fireEvent.click(screen.getByText("Подробно"));
  expect(mockApiPost).toHaveBeenCalledWith("/api/commands/menu-run", { token: "t", value: "long" });
});

it("disables buttons after a click to prevent double-submit", () => {
  render(
    <CommandArgsMenuCard
      data={{
        card_type: "command_args_menu",
        command: "summarize_video",
        text: "Выберите длину",
        token: "t",
        options: [
          { value: "short", label: "Коротко" },
          { value: "long", label: "Подробно" },
        ],
      }}
    />
  );
  fireEvent.click(screen.getByText("Подробно"));
  expect(screen.getByText("Коротко").closest("button")).toBeDisabled();
  expect(screen.getByText("Подробно").closest("button")).toBeDisabled();
});

it("does not POST when token is absent", () => {
  render(
    <CommandArgsMenuCard
      data={{
        card_type: "command_args_menu",
        command: "summarize_video",
        text: "Выберите длину",
        options: [{ value: "short", label: "Коротко" }],
      }}
    />
  );
  fireEvent.click(screen.getByText("Коротко"));
  expect(mockApiPost).not.toHaveBeenCalled();
});
