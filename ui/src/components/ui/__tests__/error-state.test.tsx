import { test, expect } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render, screen } from "@testing-library/react"
import { ErrorState } from "../error-state"

test("renders the message text", () => {
  render(<ErrorState message="Something went wrong" />)
  expect(screen.getByText("Something went wrong")).toBeInTheDocument()
})

test("renders default AlertTriangle icon when icon omitted", () => {
  const { container } = render(<ErrorState message="Error" />)
  const svg = container.querySelector("svg")
  expect(svg).toBeInTheDocument()
})

test("renders a passed action node", () => {
  render(
    <ErrorState message="Error" action={<button>Retry</button>} />
  )
  expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument()
})

test("applies a passed className to the container", () => {
  const { container } = render(
    <ErrorState message="Error" className="min-h-dvh" />
  )
  const div = container.firstChild as HTMLElement
  expect(div).toHaveClass("min-h-dvh")
  expect(div).toHaveClass("flex")
  expect(div).toHaveClass("flex-1")
})
