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

  it("uses flexible height (40vh) with a 280px minimum", () => {
    const { container } = render(<TerminalView baseUrl="http://localhost" sessionId={null} />);
    const body = container.querySelector(".term-body") as HTMLElement | null;
    expect(body).not.toBeNull();
    expect(body!.style.minHeight).toBe("280px");
    expect(body!.style.height).toBe("40vh");
    expect(body!.style.width).toBe("100%");
  });
});
