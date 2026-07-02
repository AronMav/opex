import { test, expect, vi } from "vitest"
import "@testing-library/jest-dom/vitest"
import { render, screen, fireEvent } from "@testing-library/react"
import { SearchInput } from "../search-input"

test("emits on change (no debounce)", () => {
  const onChange = vi.fn()
  render(<SearchInput value="" onChange={onChange} placeholder="Search" />)
  fireEvent.change(screen.getByPlaceholderText("Search"), { target: { value: "abc" } })
  expect(onChange).toHaveBeenCalledWith("abc")
})

test("clear button empties and emits empty", () => {
  const onChange = vi.fn()
  render(<SearchInput value="x" onChange={onChange} placeholder="Search" />)
  fireEvent.click(screen.getByRole("button", { name: /clear/i }))
  expect(onChange).toHaveBeenCalledWith("")
})
