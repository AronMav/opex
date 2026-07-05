import { test, expect, vi } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render, screen, fireEvent } from "@testing-library/react"
import { DialogTabs, type DialogTabItem } from "../dialog-tabs"

function Dot({ className }: { className?: string }) {
  return <span data-testid="icon" className={className} />
}

const ITEMS: DialogTabItem<"general" | "tools">[] = [
  { value: "general", label: "Общее", icon: Dot },
  { value: "tools", label: "Инструменты", icon: Dot },
]

test("renders one tab button per item with an icon", () => {
  render(<DialogTabs items={ITEMS} value="general" onChange={() => {}} />)
  expect(screen.getAllByRole("tab")).toHaveLength(2)
  expect(screen.getAllByTestId("icon")).toHaveLength(2)
})

test("marks the active tab via aria-selected", () => {
  render(<DialogTabs items={ITEMS} value="tools" onChange={() => {}} />)
  expect(screen.getByRole("tab", { name: "Инструменты" })).toHaveAttribute("aria-selected", "true")
  expect(screen.getByRole("tab", { name: "Общее" })).toHaveAttribute("aria-selected", "false")
})

test("fires onChange with the clicked tab value", () => {
  const onChange = vi.fn()
  render(<DialogTabs items={ITEMS} value="general" onChange={onChange} />)
  fireEvent.click(screen.getByRole("tab", { name: "Инструменты" }))
  expect(onChange).toHaveBeenCalledWith("tools")
})

test("active label is always visible; inactive label collapses on mobile", () => {
  render(<DialogTabs items={ITEMS} value="general" onChange={() => {}} />)
  const active = screen.getByText("Общее")
  const inactive = screen.getByText("Инструменты")
  expect(active).toHaveClass("inline")
  expect(active).not.toHaveClass("hidden")
  expect(inactive).toHaveClass("hidden")
  expect(inactive).toHaveClass("sm:inline")
})
