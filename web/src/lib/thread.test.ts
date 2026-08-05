import { describe, expect, it } from "vitest";

import type { HNHit } from "@/lib/meili";
import { buildTree, countDescendants } from "@/lib/thread";

/** Minimal comment document; only the fields buildTree reads matter. */
function comment(id: number, parent: number, created_at = id): HNHit {
  return {
    id,
    type: "comment",
    tags: ["comment"],
    author: `u${id}`,
    points: 0,
    num_comments: 0,
    created_at,
    parent,
  };
}

describe("buildTree", () => {
  it("nests replies under their parent and assigns depth", () => {
    const tree = buildTree(1, [comment(2, 1), comment(3, 2), comment(4, 3)]);

    expect(tree).toHaveLength(1);
    expect(tree[0].id).toBe(2);
    expect(tree[0].depth).toBe(0);
    expect(tree[0].children[0].id).toBe(3);
    expect(tree[0].children[0].depth).toBe(1);
    expect(tree[0].children[0].children[0].id).toBe(4);
    expect(tree[0].children[0].children[0].depth).toBe(2);
  });

  it("orders siblings by created_at, then by id as a tiebreak", () => {
    const tree = buildTree(1, [
      comment(4, 1, 300),
      comment(3, 1, 100),
      comment(2, 1, 100),
    ]);

    expect(tree.map((n) => n.id)).toEqual([2, 3, 4]);
  });

  it("drops comments whose parent is not reachable from the root", () => {
    // 5 hangs off 99, which was never indexed — the deleted-parent case.
    const tree = buildTree(1, [comment(2, 1), comment(5, 99)]);

    expect(tree.map((n) => n.id)).toEqual([2]);
  });

  it("returns an empty tree when nothing replies to the root", () => {
    expect(buildTree(1, [])).toEqual([]);
  });

  it("terminates on a cyclic parent chain without revisiting a node", () => {
    // 2 -> 3 -> 2 : the duplicate must be dropped, not followed forever.
    const tree = buildTree(1, [comment(2, 1), comment(3, 2), comment(2, 3)]);

    expect(tree).toHaveLength(1);
    expect(tree[0].id).toBe(2);
    expect(tree[0].children[0].id).toBe(3);
    expect(tree[0].children[0].children).toEqual([]);
  });
});

describe("countDescendants", () => {
  it("counts every node beneath a node, not just direct children", () => {
    const tree = buildTree(1, [
      comment(2, 1),
      comment(3, 2),
      comment(4, 3),
      comment(5, 2),
    ]);

    expect(countDescendants(tree[0])).toBe(3);
  });

  it("returns zero for a leaf", () => {
    const tree = buildTree(1, [comment(2, 1)]);

    expect(countDescendants(tree[0])).toBe(0);
  });
});
