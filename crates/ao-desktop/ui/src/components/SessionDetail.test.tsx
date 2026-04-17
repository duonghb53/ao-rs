import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { DashboardSession } from "../lib/types";
import { SessionDetail } from "./SessionDetail";

function makeSession(partial: Partial<DashboardSession> = {}): DashboardSession {
  return {
    id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
    projectId: "ao-rs",
    status: "working",
    activity: "idle working",
    agent: null,
    branch: "ao-fb01ba28-feat-issue-77",
    summary: null,
    summaryIsFallback: false,
    issueTitle: null,
    issueId: null,
    issueUrl: null,
    userPrompt: null,
    pr: null,
    attentionLevel: "working",
    metadata: {},
    ...partial,
  };
}

function renderDetail(overrides: Partial<Parameters<typeof SessionDetail>[0]> = {}) {
  const onSendMessage = vi.fn().mockResolvedValue(undefined);
  const onKill = vi.fn().mockResolvedValue(undefined);
  const onRestore = vi.fn().mockResolvedValue(undefined);
  const utils = render(
    <SessionDetail
      session={makeSession()}
      onSendMessage={onSendMessage}
      onKill={onKill}
      onRestore={onRestore}
      {...overrides}
    />,
  );
  return { ...utils, onSendMessage, onKill, onRestore };
}

describe("SessionDetail message shortcut", () => {
  afterEach(() => {
    cleanup();
  });

  it("sends the message on Cmd+Enter", () => {
    const { onSendMessage } = renderDetail();
    const textarea = screen.getByPlaceholderText("Type a message to the agent…") as HTMLTextAreaElement;

    fireEvent.change(textarea, { target: { value: "hello agent" } });
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });

    expect(onSendMessage).toHaveBeenCalledTimes(1);
    expect(onSendMessage).toHaveBeenCalledWith("hello agent");
  });

  it("sends the message on Ctrl+Enter", () => {
    const { onSendMessage } = renderDetail();
    const textarea = screen.getByPlaceholderText("Type a message to the agent…") as HTMLTextAreaElement;

    fireEvent.change(textarea, { target: { value: "ship it" } });
    fireEvent.keyDown(textarea, { key: "Enter", ctrlKey: true });

    expect(onSendMessage).toHaveBeenCalledTimes(1);
    expect(onSendMessage).toHaveBeenCalledWith("ship it");
  });

  it("does not send on plain Enter", () => {
    const { onSendMessage } = renderDetail();
    const textarea = screen.getByPlaceholderText("Type a message to the agent…") as HTMLTextAreaElement;

    fireEvent.change(textarea, { target: { value: "multi\nline" } });
    fireEvent.keyDown(textarea, { key: "Enter" });

    expect(onSendMessage).not.toHaveBeenCalled();
  });

  it("does not send when message is empty or whitespace", () => {
    const { onSendMessage } = renderDetail();
    const textarea = screen.getByPlaceholderText("Type a message to the agent…") as HTMLTextAreaElement;

    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });
    expect(onSendMessage).not.toHaveBeenCalled();

    fireEvent.change(textarea, { target: { value: "   " } });
    fireEvent.keyDown(textarea, { key: "Enter", ctrlKey: true });
    expect(onSendMessage).not.toHaveBeenCalled();
  });
});
