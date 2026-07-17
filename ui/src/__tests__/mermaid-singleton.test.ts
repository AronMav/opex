import { describe, it, expect, vi, beforeEach } from "vitest"

const initialize = vi.fn()
vi.mock("mermaid", () => ({ default: { initialize, render: vi.fn() } }))

describe("mermaid singleton", () => {
  beforeEach(() => {
    vi.resetModules()
    initialize.mockClear()
  })

  it("initializes once per theme, remaps light->neutral", async () => {
    const { getMermaid } = await import("@/lib/mermaid-singleton")
    await getMermaid("light")
    await getMermaid("light") // second block, same theme
    expect(initialize).toHaveBeenCalledTimes(1)
    expect(initialize.mock.calls[0][0]).toMatchObject({ theme: "neutral", securityLevel: "strict", startOnLoad: false })
    await getMermaid("dark")
    expect(initialize).toHaveBeenCalledTimes(2)
    expect(initialize.mock.calls[1][0]).toMatchObject({ theme: "dark" })
  })

  it("single-flight: parallel calls initialize once", async () => {
    const { getMermaid } = await import("@/lib/mermaid-singleton")
    await Promise.all([getMermaid("light"), getMermaid("light"), getMermaid("light")])
    expect(initialize).toHaveBeenCalledTimes(1)
  })

  it("failed init is not cached: next call retries and can succeed", async () => {
    const { getMermaid } = await import("@/lib/mermaid-singleton")
    initialize.mockImplementationOnce(() => {
      throw new Error("init boom")
    })
    await expect(getMermaid("light")).rejects.toThrow("init boom")
    // Rejection must not stay cached — a retry must call initialize again and resolve.
    await expect(getMermaid("light")).resolves.toBeDefined()
    expect(initialize).toHaveBeenCalledTimes(2)
    // And the successful init IS cached afterwards.
    await getMermaid("light")
    expect(initialize).toHaveBeenCalledTimes(2)
  })
})
