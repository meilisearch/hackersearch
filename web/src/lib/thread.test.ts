import { describe, expect, it } from "vitest";

import type { HNHit } from "@/lib/meili";
import {
  buildTree,
  chunkFrontier,
  countDescendants,
  fetchThread,
  FRONTIER_CHUNK,
  MAX_COMMENTS,
  MAX_DEPTH,
  type LevelSearch,
} from "@/lib/thread";

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

/**
 * A LevelSearch backed by a fixed parent -> children map, so a walk can be
 * driven deterministically. Records every call for assertions.
 */
function stubSearch(childrenOf: Record<number, HNHit[]>) {
  const calls: { parentIds: number[]; page: number }[] = [];
  const search: LevelSearch = async (parentIds, page) => {
    calls.push({ parentIds, page });
    const hits = parentIds.flatMap((id) => childrenOf[id] ?? []);
    return { hits, totalPages: 1 };
  };
  return { search, calls };
}

describe("chunkFrontier", () => {
  it("returns one chunk when the frontier fits", () => {
    expect(chunkFrontier([1, 2, 3], 500)).toEqual([[1, 2, 3]]);
  });

  it("returns no chunks for an empty frontier", () => {
    expect(chunkFrontier([], 500)).toEqual([]);
  });

  it("does not split at exactly the chunk size", () => {
    const ids = Array.from({ length: FRONTIER_CHUNK }, (_, i) => i + 1);

    expect(chunkFrontier(ids)).toHaveLength(1);
  });

  it("splits one past the chunk size, leaving a single trailing id", () => {
    const ids = Array.from({ length: FRONTIER_CHUNK + 1 }, (_, i) => i + 1);
    const chunks = chunkFrontier(ids);

    expect(chunks).toHaveLength(2);
    expect(chunks[0]).toHaveLength(FRONTIER_CHUNK);
    expect(chunks[1]).toEqual([FRONTIER_CHUNK + 1]);
  });
});

describe("fetchThread", () => {
  it("walks until a level comes back empty", async () => {
    const { search } = stubSearch({
      1: [comment(2, 1), comment(3, 1)],
      2: [comment(4, 2)],
    });

    const result = await fetchThread(1, { search });

    expect(result.comments.map((c) => c.id)).toEqual([2, 3, 4]);
    expect(result.depth).toBe(2);
    expect(result.truncated).toBe(false);
    expect(result.error).toBeUndefined();
  });

  it("reports each level through onLevel as it completes", async () => {
    const { search } = stubSearch({
      1: [comment(2, 1)],
      2: [comment(3, 2)],
    });
    const levels: number[][] = [];

    await fetchThread(1, { search, onLevel: (l) => levels.push(l.map((c) => c.id)) });

    expect(levels).toEqual([[2], [3]]);
  });

  it("returns an empty result for a thread with no replies", async () => {
    const { search } = stubSearch({});

    const result = await fetchThread(1, { search });

    expect(result.comments).toEqual([]);
    expect(result.depth).toBe(0);
    expect(result.truncated).toBe(false);
  });

  it("stops at MAX_DEPTH and marks the result truncated", async () => {
    // An unbounded chain: every comment has exactly one reply, forever.
    let next = 2;
    const search: LevelSearch = async (parentIds) => ({
      hits: parentIds.map((id) => comment(next++, id)),
      totalPages: 1,
    });

    const result = await fetchThread(1, { search });

    expect(result.depth).toBe(MAX_DEPTH);
    expect(result.truncated).toBe(true);
  });

  it("stops once MAX_COMMENTS is reached and marks the result truncated", async () => {
    // A single very wide level. This also pins the documented overshoot: a
    // level in flight always completes, so the cap is a floor, not a ceiling.
    let next = 2;
    const search: LevelSearch = async (parentIds) => ({
      hits: Array.from({ length: MAX_COMMENTS + 50 }, () =>
        comment(next++, parentIds[0]),
      ),
      totalPages: 1,
    });

    const result = await fetchThread(1, { search });

    expect(result.comments).toHaveLength(MAX_COMMENTS + 50);
    expect(result.truncated).toBe(true);
    expect(result.depth).toBe(1);
  });

  it("keeps the comments already gathered when a level query fails", async () => {
    let call = 0;
    const search: LevelSearch = async (parentIds) => {
      call += 1;
      if (call === 1) return { hits: [comment(2, parentIds[0])], totalPages: 1 };
      throw new Error("meilisearch exploded");
    };

    const result = await fetchThread(1, { search });

    expect(result.comments.map((c) => c.id)).toEqual([2]);
    expect(result.error?.message).toBe("meilisearch exploded");
    // A failure is not truncation — the distinction drives different UI.
    expect(result.truncated).toBe(false);
  });

  it("pages through a level that spans multiple pages", async () => {
    const search: LevelSearch = async (parentIds, page) => {
      if (parentIds[0] !== 1) return { hits: [], totalPages: 1 };
      return { hits: [comment(page + 10, 1)], totalPages: 3 };
    };

    const result = await fetchThread(1, { search });

    expect(result.comments.map((c) => c.id)).toEqual([11, 12, 13]);
  });

  it("splits a wide level into chunked queries", async () => {
    const wide = Array.from({ length: FRONTIER_CHUNK + 10 }, (_, i) => comment(i + 2, 1));
    const { search, calls } = stubSearch({ 1: wide });

    await fetchThread(1, { search });

    // Level 1: one call. Level 2: the 510-wide frontier splits into two.
    expect(calls.map((c) => c.parentIds.length)).toEqual([1, FRONTIER_CHUNK, 10]);
  });

  it("measures elapsed time with the injected clock", async () => {
    const { search } = stubSearch({ 1: [comment(2, 1)] });
    const ticks = [1000, 1042];
    let i = 0;

    const result = await fetchThread(1, { search, now: () => ticks[i++] });

    expect(result.elapsedMs).toBe(42);
  });
});
