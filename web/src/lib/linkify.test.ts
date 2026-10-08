import { describe, expect, it } from "vitest";

import { linkify } from "@/lib/linkify";

describe("linkify", () => {
  it("returns plain text untouched", () => {
    expect(linkify("no links here")).toEqual([{ kind: "text", text: "no links here" }]);
  });

  it("links full URLs and keeps the surrounding text", () => {
    expect(linkify("See https://example.com/a?b=1 for more")).toEqual([
      { kind: "text", text: "See " },
      { kind: "link", url: "https://example.com/a?b=1" },
      { kind: "text", text: " for more" },
    ]);
  });

  it("drops sentence punctuation glued to a URL", () => {
    expect(linkify("Read https://example.com/post.")).toEqual([
      { kind: "text", text: "Read " },
      { kind: "link", url: "https://example.com/post" },
      { kind: "text", text: "." },
    ]);
  });

  it("keeps balanced parens and drops an unopened closing one", () => {
    expect(linkify("(https://en.wikipedia.org/wiki/Rust_(language))")).toEqual([
      { kind: "text", text: "(" },
      { kind: "link", url: "https://en.wikipedia.org/wiki/Rust_(language)" },
      { kind: "text", text: ")" },
    ]);
    expect(linkify("(see https://example.com)")).toEqual([
      { kind: "text", text: "(see " },
      { kind: "link", url: "https://example.com" },
      { kind: "text", text: ")" },
    ]);
  });

  it("leaves HN's truncated display URLs as text", () => {
    const text = "list: https://gist.github.com/q3k/af3d93b6a1f399de28fe1... ok";
    expect(linkify(text)).toEqual([{ kind: "text", text }]);
  });
});
