import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { CommandArgsMenuCard } from "./command-args-menu-card";

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
