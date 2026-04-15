import { describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { App } from "./App";

vi.mock("../components/TerminalView", () => {
  return {
    default: function TerminalViewMock() {
      return null;
    },
  };
});

vi.mock("../api/client", () => {
  const sessions = [
    { id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", project_id: "my-app", issue_id: "42", status: "pr_open", activity: "work" },
    { id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", project_id: "my-app", issue_id: "59", status: "working", activity: "work" },
  ];

  return {
    connectEvents: () => ({ close() {} }),
    getSessions: async () => sessions,
    killSession: async () => {},
    restoreSession: async () => sessions[0],
    sendMessage: async () => {},
  };
});

describe("App session tabs", () => {
  it("shows the session detail for the active session tab", async () => {
    const user = userEvent.setup();
    const { container } = render(<App />);

    // Wait until at least one session card renders.
    const terminalPills = await screen.findAllByText("terminal");

    // Open session A, then session B.
    await user.click(terminalPills[0]);
    await user.click(terminalPills[1]);

    // Click the tab for session A.
    const tabsRegion = screen.getByText("Dashboard").closest("section");
    expect(tabsRegion).not.toBeNull();
    const tabButtonA = await within(tabsRegion!).findByRole("button", { name: "my-app - #42: pr_open" });
    await user.click(tabButtonA);

    // The Session Detail hero shows the active session id prefix (not just the tab label).
    const heroMono = container.querySelector(".detail-hero__sub .mono");
    expect(heroMono).not.toBeNull();
    expect(heroMono).toHaveTextContent("aaaaaaaa");
    expect(heroMono).not.toHaveTextContent("bbbbbbbb");
  });
});

