import { describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { App } from "./App";

vi.mock("../components/TerminalView", () => {
  return {
    default: function TerminalViewMock() {
      return null;
    },
  };
});

type EventHandlers = { onEvent?: (evt: unknown) => void; onOpen?: () => void };
const sseHandlers: { current: EventHandlers } = { current: {} };

vi.mock("../api/client", () => {
  const sessions = [
    { id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", project_id: "my-app", issue_id: "42", status: "pr_open", activity: "work" },
    { id: "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", project_id: "my-app", issue_id: "59", status: "working", activity: "work" },
  ];

  return {
    connectEvents: (_url: string, handlers: EventHandlers) => {
      sseHandlers.current = handlers;
      return { close() {} };
    },
    getSessions: async () => sessions,
    killSession: async () => {},
    restoreSession: async () => sessions[0],
    sendMessage: async () => {},
  };
});

describe("topbar active counter", () => {
  it("shows the count of non-terminal sessions next to the connection pill", async () => {
    render(<App />);

    // Both mocked sessions (pr_open, working) are non-terminal.
    const counter = await screen.findByLabelText("2 active sessions");
    expect(counter).toHaveTextContent("2 active");
  });
});

describe("App session tabs", () => {
  it("shows the session detail for the active session tab", async () => {
    const user = userEvent.setup();
    const { container } = render(<App />);

    // Open session B first (in Working column), so switching away leaves the
    // Dashboard view. Then re-open the Dashboard tab and open session A.
    const terminalPillsInitial = await screen.findAllByText("terminal");
    await user.click(terminalPillsInitial[0]);

    const dashboardTab = await screen.findByRole("tab", { name: "dashboard" });
    await user.click(dashboardTab);

    const terminalPills = await screen.findAllByText("terminal");
    // First card in DOM is the working-lane session (B); second is the review-lane (A).
    await user.click(terminalPills[1]);

    const tabButtonA = await screen.findByRole("tab", { name: "my-app - #42: pr_open" });
    await user.click(tabButtonA);

    const heroMono = container.querySelector(".sess-head .meta-row .mono");
    expect(heroMono).not.toBeNull();
    expect(heroMono).toHaveTextContent("aaaaaaaa");
    expect(heroMono).not.toHaveTextContent("bbbbbbbb");
  });
});

describe("toast dismiss", () => {
  it("removes the toast when the × button is clicked without opening the session", async () => {
    const user = userEvent.setup();
    render(<App />);

    await screen.findAllByText("terminal");

    act(() => {
      sseHandlers.current.onEvent?.({
        type: "ui_notification",
        notification: {
          id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
          reaction_key: "needs-review",
          action: "review_pr",
          priority: "action",
          message: "PR ready for review",
        },
      });
    });

    const dismiss = await screen.findByRole("button", { name: "Dismiss" });
    expect(screen.getByText("needs-review")).toBeInTheDocument();

    await user.click(dismiss);

    expect(screen.queryByText("needs-review")).not.toBeInTheDocument();
    // Clicking dismiss must NOT open the session tab for that id.
    expect(screen.queryByRole("button", { name: "my-app - #42: pr_open" })).toBeNull();
  });
});

