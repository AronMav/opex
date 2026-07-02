import { test, expect } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render, screen } from "@testing-library/react"
import { Tabs, TabsTrigger } from "@/components/ui/tabs"
import { ScrollableTabsList } from "../scrollable-tabs-list"

test("renders tab triggers and the tablist is horizontally scrollable", () => {
  render(
    <Tabs defaultValue="a">
      <ScrollableTabsList>
        <TabsTrigger value="a">A</TabsTrigger>
        <TabsTrigger value="b">B</TabsTrigger>
      </ScrollableTabsList>
    </Tabs>,
  )
  expect(screen.getByRole("tab", { name: "A" })).toBeInTheDocument()
  expect(screen.getByRole("tab", { name: "B" })).toBeInTheDocument()
  expect(screen.getByRole("tablist")).toHaveClass("overflow-x-auto")
})
