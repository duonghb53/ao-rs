import { describe, expect, it } from "vitest";

import { projectAccentStyle, projectHue } from "./projectColors";

describe("projectColors", () => {
  it("is deterministic for the same projectId", () => {
    const a = projectHue("ao-rs");
    const b = projectHue("ao-rs");
    expect(a).toBe(b);

    const styleA = projectAccentStyle("ao-rs");
    const styleB = projectAccentStyle("ao-rs");
    expect(styleA["--project-h"]).toBe(styleB["--project-h"]);
  });

  it("produces a hue in range", () => {
    const hue = projectHue("ao-rs");
    expect(hue).toBeGreaterThanOrEqual(0);
    expect(hue).toBeLessThan(360);
  });

  it("changes hue across different projectIds", () => {
    const h1 = projectHue("ao-rs");
    const h2 = projectHue("ao-desktop");
    // Collision is theoretically possible due to modulo, but extremely unlikely.
    expect(h1).not.toBe(h2);
  });
});

