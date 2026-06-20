import { render, screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import { SlashMenu } from "@/app/(authenticated)/chat/parts/SlashMenu";

describe("SlashMenu", () => {
  it("offers /compact when typing /comp", () => {
    render(<SlashMenu query="/comp" onSelect={() => {}} onClose={() => {}} />);
    expect(screen.queryByText("/compact")).not.toBeNull();
  });

  it("hides /compact when query does not match", () => {
    render(<SlashMenu query="/think" onSelect={() => {}} onClose={() => {}} />);
    expect(screen.queryByText("/compact")).toBeNull();
  });
});
