import { describe, expect, it } from "vitest";

import { FRONT_PAGE_REQUEST, isFrontPage } from "@/lib/front-page";

describe("isFrontPage", () => {
  it("matches the arrival search whatever rail sections are open", () => {
    expect(isFrontPage(FRONT_PAGE_REQUEST)).toBe(true);
    expect(isFrontPage({ ...FRONT_PAGE_REQUEST, openFacets: [] })).toBe(true);
  });

  it.each([
    ["a query", { q: "rust" }],
    ["the comments tab", { scope: "comments" as const }],
    ["another sort", { sort: "date" as const }],
    ["another page", { page: 2 }],
    ["a filter", { tags: ["show_hn"] }],
    ["a time range", { dateRange: "week" as const }],
  ])("does not match with %s", (_, patch) => {
    expect(isFrontPage({ ...FRONT_PAGE_REQUEST, ...patch })).toBe(false);
  });
});
