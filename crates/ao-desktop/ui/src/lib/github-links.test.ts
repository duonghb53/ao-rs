import { describe, expect, it } from "vitest";
import { buildCompareUrl } from "./github-links";

describe("buildCompareUrl", () => {
  it("builds correct compare URL", () => {
    expect(buildCompareUrl("owner", "repo", "main", "feat/fix")).toBe(
      "https://github.com/owner/repo/compare/main...feat/fix"
    );
  });

  it("handles branch names with slashes", () => {
    expect(buildCompareUrl("acme", "myapp", "main", "feature/ISSUE-123")).toBe(
      "https://github.com/acme/myapp/compare/main...feature/ISSUE-123"
    );
  });

  it("handles non-main base branches", () => {
    expect(buildCompareUrl("org", "repo", "develop", "hotfix/critical")).toBe(
      "https://github.com/org/repo/compare/develop...hotfix/critical"
    );
  });
});
