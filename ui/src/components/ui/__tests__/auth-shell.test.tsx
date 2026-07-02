import { test, expect } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render, screen } from "@testing-library/react"
import { AuthShell, AuthBrand } from "../auth-shell"

test("AuthShell renders its children", () => {
  render(<AuthShell><span data-testid="child">content</span></AuthShell>)
  expect(screen.getByTestId("child")).toBeInTheDocument()
})

test("glow defaults off (no decorative glow div)", () => {
  const { container } = render(<AuthShell data-testid="shell" />)
  const glow = container.querySelector('[aria-hidden="true"]')
  expect(glow).not.toBeInTheDocument()
})

test("glow prop renders the aria-hidden decorative layer", () => {
  const { container } = render(<AuthShell glow data-testid="shell" />)
  const glow = container.querySelector('[aria-hidden="true"]')
  expect(glow).toBeInTheDocument()
  expect(glow).toHaveAttribute("aria-hidden", "true")
})

test("AuthBrand vertical renders OPEX text", () => {
  render(<AuthBrand orientation="vertical" />)
  expect(screen.getByText("OPEX")).toBeInTheDocument()
})

test("AuthBrand horizontal renders OPEX text", () => {
  render(<AuthBrand orientation="horizontal" />)
  expect(screen.getByText("OPEX")).toBeInTheDocument()
})

test("AuthBrand vertical with subtitle renders subtitle", () => {
  render(<AuthBrand orientation="vertical" subtitle="hello" />)
  expect(screen.getByText("hello")).toBeInTheDocument()
})

test("AuthBrand horizontal does not render subtitle slot", () => {
  render(<AuthBrand orientation="horizontal" subtitle="hello" />)
  expect(screen.queryByText("hello")).not.toBeInTheDocument()
})
