import { test, expect } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render, screen } from "@testing-library/react"
import { Chip } from "../chip"

test("renders its content", () => {
  render(<Chip>push</Chip>)
  expect(screen.getByText("push")).toBeInTheDocument()
})

test("primary tone applies the primary text token", () => {
  render(<Chip tone="primary">tool</Chip>)
  expect(screen.getByText("tool")).toHaveClass("text-primary")
})
