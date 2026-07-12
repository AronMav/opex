import { render, screen } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { NotificationInfraBody } from "./notification-infra-body";

vi.mock("@/lib/queries", () => ({
  useResolveInfraDecision: () => ({ mutate: vi.fn(), isPending: false }),
}));

describe("NotificationInfraBody", () => {
  it("рендерит кнопки да/нет при наличии decision_id", () => {
    const n = { type: "infra_decision", data: { decision_id: "abc" } } as never;
    render(<NotificationInfraBody n={n} />);
    expect(screen.getByText("Выполнить")).toBeTruthy();
    expect(screen.getByText("Отклонить")).toBeTruthy();
  });

  it("ничего не рендерит без decision_id", () => {
    const n = { type: "infra_decision", data: {} } as never;
    const { container } = render(<NotificationInfraBody n={n} />);
    expect(container.firstChild).toBeNull();
  });
});
