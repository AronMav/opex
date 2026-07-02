import { test, expect } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render, screen } from "@testing-library/react"
import { Alert } from "../alert"

test("renders children and has role='alert'", () => {
  render(<Alert>Test alert message</Alert>)
  const alert = screen.getByRole("alert")
  expect(alert).toBeInTheDocument()
  expect(alert).toHaveTextContent("Test alert message")
})

test("variant='destructive' applies destructive text token", () => {
  render(<Alert variant="destructive">Error occurred</Alert>)
  const alert = screen.getByRole("alert")
  expect(alert).toHaveClass("text-destructive")
})

test("default variant is 'info' with correct classes", () => {
  render(<Alert>Info message</Alert>)
  const alert = screen.getByRole("alert")
  expect(alert).toHaveClass("text-foreground")
  expect(alert).toHaveClass("bg-muted/40")
})

test("renders icon when icon prop is provided", () => {
  const TestIcon = () => <span data-testid="test-icon">📌</span>
  render(
    <Alert icon={<TestIcon />}>
      Alert with icon
    </Alert>
  )
  expect(screen.getByTestId("test-icon")).toBeInTheDocument()
})

test("does not render icon wrapper when icon prop is omitted", () => {
  const { container } = render(<Alert>Alert without icon</Alert>)
  const iconSpan = container.querySelector(".shrink-0")
  expect(iconSpan).not.toBeInTheDocument()
})

test("success variant applies correct classes", () => {
  render(<Alert variant="success">Success message</Alert>)
  const alert = screen.getByRole("alert")
  expect(alert).toHaveClass("text-success")
  expect(alert).toHaveClass("bg-success/10")
})

test("warning variant applies correct classes", () => {
  render(<Alert variant="warning">Warning message</Alert>)
  const alert = screen.getByRole("alert")
  expect(alert).toHaveClass("text-warning")
  expect(alert).toHaveClass("bg-warning/10")
})
