import "@testing-library/jest-dom/vitest";

if (!("matchMedia" in window)) {
  // Minimal matchMedia polyfill for libraries like xterm.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (window as any).matchMedia = (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  });
}

if (!HTMLCanvasElement.prototype.getContext) {
  // jsdom doesn't implement canvas; some deps probe it.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  HTMLCanvasElement.prototype.getContext = (() => null) as any;
}

