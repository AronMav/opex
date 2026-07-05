import { test, expect } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render, screen } from "@testing-library/react"
import { Sheet, SheetContent, SheetBody } from "../sheet"

test("SheetBody is an internal scroll region", () => {
  render(
    <Sheet open>
      <SheetContent>
        <SheetBody>content</SheetBody>
      </SheetContent>
    </Sheet>,
  )
  const body = screen.getByText("content")
  expect(body).toHaveClass("flex-1")
  expect(body).toHaveClass("min-h-0")
  expect(body).toHaveClass("overflow-y-auto")
})
