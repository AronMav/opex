import { test, expect } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render, screen } from "@testing-library/react"
import { PageContainer } from "../page-container"

test("default scroll variant fills and scrolls with standard padding", () => {
  render(<PageContainer>body</PageContainer>)
  const el = screen.getByText("body")
  expect(el).toHaveClass("flex-1")
  expect(el).toHaveClass("overflow-y-auto")
  expect(el).toHaveClass("p-4")
  expect(el).toHaveClass("md:p-6")
  expect(el).toHaveClass("lg:p-8")
})

test("fill variant is a non-scrolling full-height shell", () => {
  render(<PageContainer variant="fill">body</PageContainer>)
  const el = screen.getByText("body")
  expect(el).toHaveClass("h-full")
  expect(el).toHaveClass("min-h-0")
  expect(el).toHaveClass("overflow-hidden")
  expect(el).not.toHaveClass("overflow-y-auto")
})

test("merges extra className", () => {
  render(<PageContainer className="flex flex-col gap-6">body</PageContainer>)
  const el = screen.getByText("body")
  expect(el).toHaveClass("flex-col")
  expect(el).toHaveClass("gap-6")
  expect(el).toHaveClass("flex-1")
})
