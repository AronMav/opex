import { test, expect } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render } from "@testing-library/react"
import { Stepper } from "../stepper"

test("renders one circle per step", () => {
  const steps = [
    { key: "step-1" },
    { key: "step-2" },
    { key: "step-3" },
  ]
  const { container } = render(<Stepper steps={steps} currentIndex={1} />)
  const circles = container.querySelectorAll("[class*='rounded-full']")
  expect(circles).toHaveLength(3)
})

test("step before current (index 0) shows done state with Check icon", () => {
  const steps = [
    { key: "step-1" },
    { key: "step-2" },
    { key: "step-3" },
  ]
  const { container } = render(<Stepper steps={steps} currentIndex={1} />)
  const circles = container.querySelectorAll("[class*='rounded-full']")
  const firstCircle = circles[0]
  expect(firstCircle).toHaveClass("bg-primary")
  expect(firstCircle).toHaveClass("border-primary")
  const svg = firstCircle.querySelector("svg")
  expect(svg).toBeInTheDocument()
})

test("current step (index 1) has aria-current='step'", () => {
  const steps = [
    { key: "step-1" },
    { key: "step-2" },
    { key: "step-3" },
  ]
  const { container } = render(<Stepper steps={steps} currentIndex={1} />)
  const currentStep = container.querySelector("[aria-current='step']")
  expect(currentStep).toBeInTheDocument()
  expect(currentStep).toHaveClass("border-primary")
  expect(currentStep).toHaveClass("bg-primary/10")
})

test("future step (index 2) is muted and has no aria-current", () => {
  const steps = [
    { key: "step-1" },
    { key: "step-2" },
    { key: "step-3" },
  ]
  const { container } = render(<Stepper steps={steps} currentIndex={1} />)
  const circles = container.querySelectorAll("[class*='rounded-full']")
  const futureCircle = circles[2]
  expect(futureCircle).not.toHaveAttribute("aria-current")
  expect(futureCircle).toHaveClass("border-border")
  expect(futureCircle).toHaveClass("text-muted-foreground")
})
