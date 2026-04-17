import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";

vi.mock("@xterm/xterm", () => {
  class FakeTerminal {
    cols = 80;
    rows = 24;
    loadAddon() {}
    open() {}
    focus() {}
    reset() {}
    writeln() {}
    write() {}
    onData() {
      return { dispose: () => {} };
    }
    dispose() {}
  }
  return { Terminal: FakeTerminal };
});

vi.mock("@xterm/addon-fit", () => {
  class FakeFitAddon {
    fit() {}
  }
  return { FitAddon: FakeFitAddon };
});

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

import { TerminalView } from "./TerminalView";

describe("TerminalView host sizing", () => {
  afterEach(() => {
    cleanup();
  });

  it("uses flexible height (50vh) with a 360px minimum", () => {
    const { container } = render(<TerminalView baseUrl="http://localhost" sessionId={null} />);
    const host = container.firstElementChild as HTMLElement | null;
    expect(host).not.toBeNull();
    expect(host!.style.minHeight).toBe("360px");
    expect(host!.style.height).toBe("50vh");
    expect(host!.style.width).toBe("100%");
  });
});
