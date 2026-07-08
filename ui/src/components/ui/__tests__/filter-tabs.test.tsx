import { test, expect } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render, screen } from "@testing-library/react"
import { Tabs } from "@/components/ui/tabs"
import { FilterTabsList, type FilterTabItem } from "../filter-tabs"

function Dot() {
  return <span data-testid="icon" />
}

const ITEMS: FilterTabItem[] = [
  { value: "all", label: "Все", icon: <Dot /> },
  { value: "active", label: "Активные", icon: <Dot />, count: 35 },
]

function renderBar() {
  return render(
    <Tabs defaultValue="all">
      <FilterTabsList items={ITEMS} />
    </Tabs>,
  )
}

test("renders one trigger per item with an accessible name from label", () => {
  renderBar()
  expect(screen.getByRole("tab", { name: "Все" })).toBeInTheDocument()
  expect(screen.getByRole("tab", { name: "Активные" })).toBeInTheDocument()
})

test("renders an icon for every tab", () => {
  renderBar()
  expect(screen.getAllByTestId("icon")).toHaveLength(2)
})

test("renders a count badge only when count is provided", () => {
  renderBar()
  // "Активные" carries the count; "Все" does not.
  expect(screen.getByText("35")).toBeInTheDocument()
})

test("each trigger has an aria-label so icon-only tabs stay accessible", () => {
  renderBar()
  expect(screen.getByRole("tab", { name: "Все" })).toHaveAttribute("aria-label", "Все")
})

test("labels are shown by default (adaptive collapse only kicks in when they overflow)", () => {
  // Collapse is measured at runtime via ResizeObserver; without a layout engine
  // (jsdom) the bar stays expanded, so labels render inline and readable.
  renderBar()
  const label = screen.getByText("Активные")
  expect(label).toHaveClass("truncate")
  expect(label).not.toHaveClass("hidden")
})

test("each trigger carries a title so a collapsed icon-only tab shows its name on hover", () => {
  renderBar()
  expect(screen.getByRole("tab", { name: "Все" })).toHaveAttribute("title", "Все")
})
