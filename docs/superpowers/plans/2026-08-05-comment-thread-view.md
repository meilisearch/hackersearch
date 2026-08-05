# Comment Thread View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clicking a story's comment count (or a comment's "thread" link) opens an HN-style nested comment thread inside the app, instead of leaving for news.ycombinator.com.

**Architecture:** Documents carry `parent` (immediate parent) but no root-story pointer, so a thread is resolved by walking the reply tree client-side — one Meilisearch query per depth level, `filter: parent IN [...]`. Pure logic (tree building, chunking, the walk itself) lives in `web/src/lib/thread.ts` behind injectable search/fetch functions so it is unit-testable without network. React sits on top: a `useThread` hook, a recursive `CommentNode`, and a `ThreadView` that replaces `<Results>` in the results column.

**Tech Stack:** Next.js 16 (App Router), React 19, TypeScript strict, TanStack Query v5, Tailwind v4, meilisearch-js 0.59, date-fns, lucide-react. Vitest is added by Task 1 as the only new dependency.

**Spec:** `docs/superpowers/specs/2026-08-05-post-comments-thread-view-design.md`

## Global Constraints

- **No indexer or index-settings changes.** `parent` is already filterable with equality; `created_at` is already sortable. Nothing in this plan touches `indexer/`.
- **Never use `any`.** TypeScript is strict; define real types.
- **`getDocument()` cannot be aborted.** In meilisearch 0.59, `Index.getDocument(documentId, parameters?)` takes no `extraRequestInit`, so no `AbortSignal` can be passed. Only `Index.search(query, options, extraRequestInit)` accepts one. Document reads are therefore fire-and-forget; staleness is handled by TanStack discarding results for keys it no longer observes. Do not write `getDocument(id, {}, { signal })` — it will not compile.
- **Walk limits, used verbatim:** `MAX_DEPTH = 25`, `MAX_COMMENTS = 2000`, `FRONTIER_CHUNK = 500`, `LEVEL_PAGE_SIZE = 1000`, `MAX_ANCESTOR_HOPS = 25`.
- **Thread root may be a comment.** `resolveThreadRoot` returns the highest reachable ancestor, which is normally a story but is a comment when the chain breaks. Name things `root`, never `story`.
- **A thread root is any non-comment item.** Stories, jobs and polls can all host comments; the upward walk terminates on `type !== "comment"`, not on `type === "story"`.
- **Follow existing file conventions:** `"use client"` at the top of components, `@/` path alias, `cn()` for class merging, `memo` on list-item components, comments explaining *why* not *what*.
- **Commit after every task.** No `Co-Authored-By` lines.

---

### Task 1: Vitest setup and `buildTree`

Establishes the test harness and the pure tree-building function everything else renders from.

**Files:**
- Create: `web/vitest.config.ts`
- Create: `web/src/lib/thread.ts`
- Create: `web/src/lib/thread.test.ts`
- Modify: `web/package.json` (add `vitest` devDependency and a `test` script)

**Interfaces:**
- Consumes: `HNHit` from `web/src/lib/meili.ts` (already exists).
- Produces: `ThreadNode`, `MAX_DEPTH`, `MAX_COMMENTS`, `FRONTIER_CHUNK`, `LEVEL_PAGE_SIZE`, `buildTree(rootId: number, comments: HNHit[]): ThreadNode[]`, `countDescendants(node: ThreadNode): number`.

- [ ] **Step 1: Install Vitest**

```bash
cd web && pnpm add -D vitest@^3
```

- [ ] **Step 2: Add the test script**

In `web/package.json`, add to `"scripts"`:

```json
"test": "vitest run"
```

- [ ] **Step 3: Create the Vitest config**

Create `web/vitest.config.ts`. The `@` alias must be resolved manually — Vitest does not read `tsconfig.json` paths on its own, and adding `vite-tsconfig-paths` would be a second new dependency for no gain.

```ts
import { fileURLToPath } from "node:url";

import { defineConfig } from "vitest/config";

export default defineConfig({
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  test: {
    include: ["src/**/*.test.ts"],
  },
});
```

- [ ] **Step 4: Write the failing tests**

Create `web/src/lib/thread.test.ts`:

```ts
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
      comment(2, 1, 100),
      comment(3, 1, 100),
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
```

- [ ] **Step 5: Run the tests to verify they fail**

Run: `cd web && pnpm test`
Expected: FAIL — `Failed to resolve import "@/lib/thread"`.

- [ ] **Step 6: Write the implementation**

Create `web/src/lib/thread.ts`:

```ts
import type { HNHit } from "./meili";

/** Hard stops for the downward walk. */
export const MAX_DEPTH = 25;
export const MAX_COMMENTS = 2000;
/** Ids per `parent IN [...]` filter, to keep filter strings bounded. */
export const FRONTIER_CHUNK = 500;
/** Meilisearch page size for one level chunk. */
export const LEVEL_PAGE_SIZE = 1000;

export interface ThreadNode extends HNHit {
  children: ThreadNode[];
  depth: number;
}

/**
 * Assemble a flat list of comments into a reply tree rooted at `rootId`.
 *
 * Anything not reachable from the root is dropped: the indexer skips deleted
 * and dead items, so a subtree whose parent is missing has no way back to the
 * root and cannot be placed. A `seen` set makes a malformed `parent` chain
 * terminate instead of recursing forever.
 */
export function buildTree(rootId: number, comments: HNHit[]): ThreadNode[] {
  const byParent = new Map<number, HNHit[]>();
  for (const hit of comments) {
    if (hit.parent == null) continue;
    const siblings = byParent.get(hit.parent);
    if (siblings) siblings.push(hit);
    else byParent.set(hit.parent, [hit]);
  }
  for (const siblings of byParent.values()) {
    siblings.sort((a, b) => a.created_at - b.created_at || a.id - b.id);
  }

  const seen = new Set<number>();
  const build = (parentId: number, depth: number): ThreadNode[] => {
    if (depth > MAX_DEPTH) return [];
    const out: ThreadNode[] = [];
    for (const hit of byParent.get(parentId) ?? []) {
      if (seen.has(hit.id)) continue;
      seen.add(hit.id);
      out.push({ ...hit, depth, children: build(hit.id, depth + 1) });
    }
    return out;
  };
  return build(rootId, 0);
}

/** Total nodes beneath `node` — the count shown on a collapsed subtree. */
export function countDescendants(node: ThreadNode): number {
  let total = 0;
  const stack = [...node.children];
  while (stack.length > 0) {
    const next = stack.pop();
    if (!next) break;
    total += 1;
    stack.push(...next.children);
  }
  return total;
}
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cd web && pnpm test`
Expected: PASS — 7 tests.

- [ ] **Step 8: Commit**

```bash
git add web/package.json web/pnpm-lock.yaml web/vitest.config.ts web/src/lib/thread.ts web/src/lib/thread.test.ts
git commit -m "Add Vitest and the thread tree builder"
```

---

### Task 2: Frontier chunking and the downward walk

The level-by-level walk itself, with its search function injected so the walk can be tested without network.

**Files:**
- Modify: `web/src/lib/thread.ts` (append)
- Modify: `web/src/lib/thread.test.ts` (append)

**Interfaces:**
- Consumes: `MAX_DEPTH`, `MAX_COMMENTS`, `FRONTIER_CHUNK`, `LEVEL_PAGE_SIZE` from Task 1.
- Produces:
  - `chunkFrontier(ids: number[], size?: number): number[][]`
  - `LevelSearch = (parentIds: number[], page: number, signal?: AbortSignal) => Promise<{ hits: HNHit[]; totalPages: number }>`
  - `meiliLevelSearch: LevelSearch`
  - `ThreadResult = { comments: HNHit[]; depth: number; truncated: boolean; elapsedMs: number; error?: Error }`
  - `fetchThread(rootId: number, options?: { signal?: AbortSignal; onLevel?: (level: HNHit[]) => void; search?: LevelSearch; now?: () => number }): Promise<ThreadResult>`

- [ ] **Step 1: Write the failing tests**

First **extend the existing `@/lib/thread` import** at the top of `web/src/lib/thread.test.ts` — do not add a second import statement from the same module:

```ts
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
```

Then append to the same file:

```ts
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && pnpm test`
Expected: FAIL — `chunkFrontier is not exported`.

- [ ] **Step 3: Write the implementation**

Append to `web/src/lib/thread.ts`:

```ts
import { INDEX_UID, meili } from "./meili";

/** Split a frontier so no single `parent IN [...]` filter grows unbounded. */
export function chunkFrontier(ids: number[], size = FRONTIER_CHUNK): number[][] {
  const chunks: number[][] = [];
  for (let i = 0; i < ids.length; i += size) {
    chunks.push(ids.slice(i, i + size));
  }
  return chunks;
}

export type LevelSearch = (
  parentIds: number[],
  page: number,
  signal?: AbortSignal,
) => Promise<{ hits: HNHit[]; totalPages: number }>;

/** The real Meilisearch query behind one level chunk. */
export const meiliLevelSearch: LevelSearch = async (parentIds, page, signal) => {
  const res = await meili.index(INDEX_UID).search(
    "",
    {
      filter: `parent IN [${parentIds.join(",")}]`,
      sort: ["created_at:asc"],
      hitsPerPage: LEVEL_PAGE_SIZE,
      page,
    },
    { signal },
  );
  return { hits: res.hits as HNHit[], totalPages: res.totalPages };
};

export interface ThreadResult {
  comments: HNHit[];
  /** Number of levels actually walked. */
  depth: number;
  /** A cap was hit — more replies exist than were fetched. */
  truncated: boolean;
  /** Wall time of the whole walk, in ms. */
  elapsedMs: number;
  /** Set when a level query threw; `comments` still holds what was gathered. */
  error?: Error;
}

/** Fetch one full level, chunked and paged. Chunks run in parallel. */
async function fetchLevel(
  frontier: number[],
  search: LevelSearch,
  signal?: AbortSignal,
): Promise<HNHit[]> {
  const perChunk = await Promise.all(
    chunkFrontier(frontier).map(async (chunk) => {
      const first = await search(chunk, 1, signal);
      const hits = [...first.hits];
      for (let page = 2; page <= first.totalPages; page += 1) {
        const next = await search(chunk, page, signal);
        hits.push(...next.hits);
      }
      return hits;
    }),
  );
  return perChunk.flat();
}

/**
 * Walk the reply tree downward from `rootId`, one query per depth level.
 *
 * There is no root-story pointer on documents, so this is the only way to
 * collect a thread. A level already in flight always finishes rather than
 * being cut mid-way, which makes MAX_COMMENTS a floor: the final count can
 * overshoot it by up to one level's width.
 */
export async function fetchThread(
  rootId: number,
  options: {
    signal?: AbortSignal;
    onLevel?: (level: HNHit[]) => void;
    search?: LevelSearch;
    now?: () => number;
  } = {},
): Promise<ThreadResult> {
  const {
    signal,
    onLevel,
    search = meiliLevelSearch,
    now = () => performance.now(),
  } = options;

  const startedAt = now();
  const comments: HNHit[] = [];
  let frontier = [rootId];
  let depth = 0;
  let error: Error | undefined;

  while (frontier.length > 0 && depth < MAX_DEPTH) {
    let level: HNHit[];
    try {
      level = await fetchLevel(frontier, search, signal);
    } catch (e) {
      error = e instanceof Error ? e : new Error(String(e));
      break;
    }
    if (level.length === 0) {
      frontier = [];
      break;
    }
    depth += 1;
    comments.push(...level);
    onLevel?.(level);
    if (comments.length >= MAX_COMMENTS) break;
    frontier = level.map((hit) => hit.id);
  }

  // A non-empty frontier means we stopped early by choice rather than because
  // the tree ran out. An error is a different failure and reported as such.
  const truncated = frontier.length > 0 && error === undefined;

  return { comments, depth, truncated, elapsedMs: Math.round(now() - startedAt), error };
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd web && pnpm test`
Expected: PASS — 20 tests.

- [ ] **Step 5: Check types and lint**

Run: `cd web && pnpm exec tsc --noEmit && pnpm lint`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add web/src/lib/thread.ts web/src/lib/thread.test.ts
git commit -m "Walk the reply tree level by level to collect a thread"
```

---

### Task 3: Resolve a comment to its thread root

The upward walk, so a comment found by search can open the discussion it belongs to.

**Files:**
- Modify: `web/src/lib/thread.ts` (append)
- Modify: `web/src/lib/thread.test.ts` (append)

**Interfaces:**
- Produces:
  - `MAX_ANCESTOR_HOPS = 25`
  - `DocumentFetch = (id: number) => Promise<HNHit | null>`
  - `meiliDocumentFetch: DocumentFetch`
  - `ThreadRoot = { root: HNHit; partial: boolean }`
  - `resolveThreadRoot(startId: number, options?: { fetchDocument?: DocumentFetch }): Promise<ThreadRoot | null>`

- [ ] **Step 1: Write the failing tests**

Again **extend the existing `@/lib/thread` import** rather than adding a second one — it should now also pull in `MAX_ANCESTOR_HOPS`, `resolveThreadRoot`, and `type DocumentFetch`. Then append:

```ts
function story(id: number): HNHit {
  return {
    id,
    type: "story",
    tags: ["story"],
    title: `story ${id}`,
    author: `u${id}`,
    points: 10,
    num_comments: 3,
    created_at: id,
  };
}

/** A DocumentFetch over a fixed set of documents; anything else 404s. */
function stubDocs(docs: HNHit[]): DocumentFetch {
  const byId = new Map(docs.map((d) => [d.id, d]));
  return async (id) => byId.get(id) ?? null;
}

describe("resolveThreadRoot", () => {
  it("returns the story a nested comment belongs to", async () => {
    const fetchDocument = stubDocs([story(1), comment(2, 1), comment(3, 2)]);

    const result = await resolveThreadRoot(3, { fetchDocument });

    expect(result?.root.id).toBe(1);
    expect(result?.partial).toBe(false);
  });

  it("returns a story handed to it directly", async () => {
    const fetchDocument = stubDocs([story(1)]);

    const result = await resolveThreadRoot(1, { fetchDocument });

    expect(result?.root.id).toBe(1);
    expect(result?.partial).toBe(false);
  });

  it("treats a job or poll as a valid thread root", async () => {
    const job: HNHit = { ...story(1), type: "job", tags: ["job"] };
    const fetchDocument = stubDocs([job, comment(2, 1)]);

    const result = await resolveThreadRoot(2, { fetchDocument });

    expect(result?.root.id).toBe(1);
    expect(result?.partial).toBe(false);
  });

  it("stops at the highest reachable ancestor when the chain breaks", async () => {
    // 2's parent (99) was deleted and never indexed.
    const fetchDocument = stubDocs([comment(2, 99), comment(3, 2)]);

    const result = await resolveThreadRoot(3, { fetchDocument });

    expect(result?.root.id).toBe(2);
    expect(result?.partial).toBe(true);
  });

  it("returns null when the starting item is not in the index", async () => {
    const fetchDocument = stubDocs([]);

    expect(await resolveThreadRoot(7, { fetchDocument })).toBeNull();
  });

  it("gives up after MAX_ANCESTOR_HOPS on a very deep chain", async () => {
    // A chain far longer than the hop cap, with no story at the top.
    const fetchDocument: DocumentFetch = async (id) => comment(id, id + 1);

    const result = await resolveThreadRoot(1, { fetchDocument });

    expect(result?.partial).toBe(true);
    expect(result?.root.id).toBe(1 + MAX_ANCESTOR_HOPS);
  });

  it("terminates on a cyclic ancestor chain", async () => {
    // 2 -> 3 -> 2 -> ... with no story anywhere.
    const fetchDocument = stubDocs([comment(2, 3), comment(3, 2)]);

    const result = await resolveThreadRoot(2, { fetchDocument });

    expect(result?.partial).toBe(true);
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && pnpm test`
Expected: FAIL — `resolveThreadRoot is not exported`.

- [ ] **Step 3: Write the implementation**

Append to `web/src/lib/thread.ts`:

```ts
/** Ancestor lookups before the upward walk gives up. */
export const MAX_ANCESTOR_HOPS = 25;

export type DocumentFetch = (id: number) => Promise<HNHit | null>;

/**
 * Read one document by id. meilisearch 0.59's `getDocument` takes no
 * `extraRequestInit`, so this cannot be given an AbortSignal — callers rely on
 * TanStack discarding results for keys it no longer observes.
 */
export const meiliDocumentFetch: DocumentFetch = async (id) => {
  try {
    return await meili.index(INDEX_UID).getDocument<HNHit>(id);
  } catch {
    // Deleted, dead, or simply never indexed — all the same to the caller.
    return null;
  }
};

export interface ThreadRoot {
  /** The item the thread should be rendered from. */
  root: HNHit;
  /** True when the walk stopped on a broken chain rather than a real root. */
  partial: boolean;
}

/**
 * Walk `parent` upward until a non-comment item is found — a story, job or
 * poll can all host a thread. Returns the highest reachable ancestor when the
 * chain breaks on a deleted item, so a partial thread can still be shown.
 */
export async function resolveThreadRoot(
  startId: number,
  options: { fetchDocument?: DocumentFetch } = {},
): Promise<ThreadRoot | null> {
  const { fetchDocument = meiliDocumentFetch } = options;

  let current = await fetchDocument(startId);
  if (!current) return null;

  const seen = new Set<number>([startId]);
  for (let hop = 0; hop < MAX_ANCESTOR_HOPS; hop += 1) {
    if (current.type !== "comment") return { root: current, partial: false };
    if (current.parent == null || seen.has(current.parent)) {
      return { root: current, partial: true };
    }
    seen.add(current.parent);
    const parent = await fetchDocument(current.parent);
    if (!parent) return { root: current, partial: true };
    current = parent;
  }
  return { root: current, partial: true };
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd web && pnpm test`
Expected: PASS — 27 tests.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/thread.ts web/src/lib/thread.test.ts
git commit -m "Resolve a comment upward to the thread it belongs to"
```

---

### Task 4: Thread state in the URL

`item` and `c` params, kept out of `SearchState` so opening a thread never re-keys the search query.

**Files:**
- Modify: `web/src/lib/search-state.ts` (append)
- Create: `web/src/lib/search-state.test.ts`

**Interfaces:**
- Produces:
  - `ThreadState = { rootId: number; focusId?: number }`
  - `threadToParams(thread: ThreadState | null, params: URLSearchParams): URLSearchParams` — mutates and returns `params`
  - `threadFromParams(params: URLSearchParams): ThreadState | null`

- [ ] **Step 1: Write the failing tests**

Create `web/src/lib/search-state.test.ts`:

```ts
import { describe, expect, it } from "vitest";

import {
  DEFAULT_STATE,
  paramsToState,
  stateToParams,
  threadFromParams,
  threadToParams,
  type ThreadState,
} from "@/lib/search-state";

describe("threadToParams / threadFromParams", () => {
  it("round-trips a thread without a focused comment", () => {
    const thread: ThreadState = { rootId: 44123 };
    const params = threadToParams(thread, new URLSearchParams());

    expect(params.get("item")).toBe("44123");
    expect(params.get("c")).toBeNull();
    expect(threadFromParams(params)).toEqual({ rootId: 44123 });
  });

  it("round-trips a thread with a focused comment", () => {
    const thread: ThreadState = { rootId: 44123, focusId: 44567 };
    const params = threadToParams(thread, new URLSearchParams());

    expect(threadFromParams(params)).toEqual({ rootId: 44123, focusId: 44567 });
  });

  it("writes nothing when there is no thread open", () => {
    const params = threadToParams(null, new URLSearchParams());

    expect(params.toString()).toBe("");
  });

  it("reads no thread from params that have none", () => {
    expect(threadFromParams(new URLSearchParams("q=rust"))).toBeNull();
  });

  it("rejects a non-numeric or non-positive item id", () => {
    expect(threadFromParams(new URLSearchParams("item=abc"))).toBeNull();
    expect(threadFromParams(new URLSearchParams("item=0"))).toBeNull();
    expect(threadFromParams(new URLSearchParams("item=-5"))).toBeNull();
  });

  it("ignores a malformed focus id but keeps the thread", () => {
    expect(threadFromParams(new URLSearchParams("item=44123&c=abc"))).toEqual({
      rootId: 44123,
    });
  });

  it("coexists with search params without disturbing them", () => {
    const params = stateToParams({ ...DEFAULT_STATE, q: "rust", scope: "comments" });
    threadToParams({ rootId: 44123 }, params);

    expect(paramsToState(params).q).toBe("rust");
    expect(paramsToState(params).scope).toBe("comments");
    expect(threadFromParams(params)?.rootId).toBe(44123);
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd web && pnpm test`
Expected: FAIL — `threadToParams is not exported`.

- [ ] **Step 3: Write the implementation**

Append to `web/src/lib/search-state.ts`:

```ts
/**
 * Which thread is open, if any. Deliberately NOT part of SearchState: that
 * object is the TanStack query key for searches, and folding the thread into
 * it would refetch every result each time a thread opens or closes.
 */
export interface ThreadState {
  rootId: number;
  focusId?: number;
}

const positiveId = (raw: string | null): number | null => {
  const value = Number(raw);
  return Number.isInteger(value) && value > 0 ? value : null;
};

export function threadToParams(
  thread: ThreadState | null,
  params: URLSearchParams,
): URLSearchParams {
  if (!thread) return params;
  params.set("item", String(thread.rootId));
  if (thread.focusId != null) params.set("c", String(thread.focusId));
  return params;
}

export function threadFromParams(params: URLSearchParams): ThreadState | null {
  const rootId = positiveId(params.get("item"));
  if (rootId === null) return null;
  const focusId = positiveId(params.get("c"));
  return focusId === null ? { rootId } : { rootId, focusId };
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd web && pnpm test`
Expected: PASS — 34 tests.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/search-state.ts web/src/lib/search-state.test.ts
git commit -m "Carry the open thread in the URL as item and c"
```

---

### Task 5: The `useThread` hook

Wraps the walk in TanStack Query and exposes partial levels so the view can paint progressively.

**Files:**
- Create: `web/src/hooks/use-thread.ts`

**Interfaces:**
- Consumes: `fetchThread`, `ThreadResult` (Task 2); `HNHit` from `@/lib/meili`.
- Produces: `useThread(rootId: number | null): { comments: HNHit[]; result?: ThreadResult; isPending: boolean; isError: boolean; isWalking: boolean }`.

- [ ] **Step 1: Write the hook**

This hook is not unit-tested — it is a thin binding over `fetchThread`, which Task 2 covers thoroughly, and testing it would mean pulling in React Testing Library and jsdom for no real coverage. It is verified in the browser in Task 9.

Create `web/src/hooks/use-thread.ts`:

```ts
"use client";

import { useQuery } from "@tanstack/react-query";
import { useState } from "react";

import type { HNHit } from "@/lib/meili";
import { fetchThread, type ThreadResult } from "@/lib/thread";

/**
 * Run the downward walk for one thread.
 *
 * Levels are accumulated into state as they arrive so the top of a thread is
 * readable long before a deep walk finishes; the resolved query data takes
 * over as the source of truth once the walk completes.
 */
export function useThread(rootId: number | null) {
  const [levels, setLevels] = useState<HNHit[]>([]);
  // Reset accumulated levels during render when the thread changes, so a stale
  // thread is never briefly shown under a new root. This is React's documented
  // "adjust state when props change" pattern, not an effect.
  const [trackedRoot, setTrackedRoot] = useState(rootId);
  if (trackedRoot !== rootId) {
    setTrackedRoot(rootId);
    setLevels([]);
  }

  const query = useQuery<ThreadResult>({
    queryKey: ["hn-thread", rootId],
    enabled: rootId !== null,
    // Threads are effectively immutable for a browsing session; re-walking on
    // every remount would be pure waste.
    staleTime: 5 * 60_000,
    queryFn: ({ signal }) => {
      // `enabled` already guarantees this, but narrowing beats casting.
      if (rootId === null) throw new Error("useThread: no thread is open");
      return fetchThread(rootId, {
        signal,
        onLevel: (level) => setLevels((prev) => [...prev, ...level]),
      });
    },
  });

  return {
    comments: query.data?.comments ?? levels,
    result: query.data,
    isPending: query.isPending && rootId !== null,
    isError: query.isError,
    isWalking: query.isFetching,
  };
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd web && pnpm exec tsc --noEmit`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add web/src/hooks/use-thread.ts
git commit -m "Add the useThread hook with progressive level loading"
```

---

### Task 6: The `CommentNode` component

One node of the tree, recursive and collapsible.

**Files:**
- Create: `web/src/app/comment-node.tsx`

**Interfaces:**
- Consumes: `ThreadNode`, `countDescendants` (Task 1); `hnItemUrl`, `hnUserUrl` from `@/lib/meili`.
- Produces: `CommentNode` with props `{ node: ThreadNode; collapsed: Set<number>; focusId?: number; onToggle: (id: number) => void }`.

- [ ] **Step 1: Write the component**

Two things to get right here:

Indent is applied **one step per level**, not `depth × step` — nodes render inside their parent, so margins already compound. Capping uses two static Tailwind classes rather than a runtime value, so no arbitrary-value class has to be generated at build time.

The inner function is named `CommentNodeInner`, **not** `CommentNode`. A named function expression binds its own name inside its body, so `memo(function CommentNode(){ … <CommentNode/> … })` would have the recursive JSX resolve to the raw function and silently bypass the `memo` wrapper on every child. Naming them differently makes the recursion go through the memoized outer binding.

Create `web/src/app/comment-node.tsx`:

```tsx
"use client";

import { formatDistanceToNowStrict } from "date-fns";
import { Minus, Plus } from "lucide-react";
import { memo } from "react";

import { hnItemUrl, hnUserUrl } from "@/lib/meili";
import { countDescendants, type ThreadNode } from "@/lib/thread";
import { cn } from "@/lib/utils";

interface CommentNodeProps {
  node: ThreadNode;
  collapsed: Set<number>;
  focusId?: number;
  onToggle: (id: number) => void;
}

export const CommentNode = memo(function CommentNodeInner({
  node,
  collapsed,
  focusId,
  onToggle,
}: CommentNodeProps) {
  const isCollapsed = collapsed.has(node.id);
  const isFocus = node.id === focusId;
  const hidden = isCollapsed ? countDescendants(node) : 0;
  const timeAgo = node.created_at
    ? formatDistanceToNowStrict(new Date(node.created_at * 1000), { addSuffix: true })
    : "";

  return (
    <article
      id={`c-${node.id}`}
      className={cn(
        "border-l-2 border-l-border py-1.5 pl-2 transition-colors hover:border-l-primary sm:pl-3",
        // One step of indent per level, dropped past the cap so deep
        // subthreads don't squeeze into a one-word column. Mobile caps
        // earlier than desktop because there is far less room.
        node.depth > 0 && node.depth <= 6 && "ml-3",
        node.depth > 0 && node.depth <= 8 && "sm:ml-5",
        isFocus && "border-l-primary bg-primary/10",
      )}
    >
      <div className="flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground">
        <button
          onClick={() => onToggle(node.id)}
          className="grid size-3.5 place-items-center border text-muted-foreground hover:border-primary hover:text-primary"
          aria-expanded={!isCollapsed}
          aria-label={isCollapsed ? "Expand replies" : "Collapse replies"}
        >
          {isCollapsed ? <Plus className="size-2.5" /> : <Minus className="size-2.5" />}
        </button>
        <a
          href={hnUserUrl(node.author)}
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:underline"
        >
          {node.author}
        </a>
        <span>{timeAgo}</span>
        <a
          href={hnItemUrl(node.id)}
          target="_blank"
          rel="noreferrer"
          className="hover:text-primary"
          title="View this comment on Hacker News"
        >
          ↗
        </a>
        {isCollapsed && hidden > 0 && (
          <span className="opacity-70">
            · {hidden} {hidden === 1 ? "reply" : "replies"} hidden
          </span>
        )}
      </div>

      {!isCollapsed && (
        <>
          {node.text ? (
            <p className="mt-1 text-sm leading-relaxed text-foreground/85 [overflow-wrap:anywhere]">
              {node.text}
            </p>
          ) : (
            <p className="mt-1 font-mono text-xs text-muted-foreground italic">
              [no content]
            </p>
          )}
          {node.children.map((child) => (
            <CommentNode
              key={child.id}
              node={child}
              collapsed={collapsed}
              focusId={focusId}
              onToggle={onToggle}
            />
          ))}
        </>
      )}
    </article>
  );
});

// The inner function is deliberately named differently so recursion goes
// through the memo wrapper; restore the useful name for React DevTools.
CommentNode.displayName = "CommentNode";
```

- [ ] **Step 2: Verify it compiles and lints**

Run: `cd web && pnpm exec tsc --noEmit && pnpm lint`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add web/src/app/comment-node.tsx
git commit -m "Add the recursive collapsible comment node"
```

---

### Task 7: The `ThreadView` component

Breadcrumb, root header, progress, tree, footer, and every error state.

**Files:**
- Create: `web/src/app/thread-view.tsx`

**Interfaces:**
- Consumes: `useThread` (Task 5); `CommentNode` (Task 6); `buildTree`, `meiliDocumentFetch`, `resolveThreadRoot` (Tasks 1–3); `ThreadState` (Task 4).
- Produces: `ThreadView` with props `{ rootId: number; focusId?: number; seed?: HNHit; onClose: () => void; onResolveRoot: (thread: ThreadState) => void }`.

- [ ] **Step 1: Write the component**

Create `web/src/app/thread-view.tsx`:

```tsx
"use client";

import { useQuery } from "@tanstack/react-query";
import { formatDistanceToNowStrict } from "date-fns";
import { ArrowLeft, ArrowUpRight, TriangleAlert } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";

import { Skeleton } from "@/components/ui/skeleton";
import { hnItemUrl, hnUserUrl, type HNHit } from "@/lib/meili";
import type { ThreadState } from "@/lib/search-state";
import { buildTree, meiliDocumentFetch, resolveThreadRoot } from "@/lib/thread";
import { useThread } from "@/hooks/use-thread";

import { CommentNode } from "./comment-node";

interface ThreadViewProps {
  rootId: number;
  focusId?: number;
  /** The already-loaded hit that was clicked, when there was one. An
   *  optimisation only — a deep link arrives without it. */
  seed?: HNHit;
  onClose: () => void;
  onResolveRoot: (thread: ThreadState) => void;
}

export function ThreadView({
  rootId,
  focusId,
  seed,
  onClose,
  onResolveRoot,
}: ThreadViewProps) {
  const [collapsed, setCollapsed] = useState<Set<number>>(new Set());
  useEffect(() => setCollapsed(new Set()), [rootId]);

  // A failed upward resolve is NOT a broken ancestor chain, and must not be
  // reported as one — see the banner condition below.
  const [resolveFailed, setResolveFailed] = useState(false);
  useEffect(() => setResolveFailed(false), [rootId]);

  const rootDoc = useQuery({
    queryKey: ["hn-item", rootId],
    queryFn: () => meiliDocumentFetch(rootId),
    initialData: seed,
    staleTime: 5 * 60_000,
  });

  // A deep link can point at a comment. Resolve it upward and hand the real
  // root back so the URL and the walk both move to the top of the thread.
  useEffect(() => {
    if (rootDoc.data?.type !== "comment") return;
    let cancelled = false;
    resolveThreadRoot(rootId)
      .then((resolved) => {
        if (cancelled || !resolved || resolved.root.id === rootId) return;
        onResolveRoot({ rootId: resolved.root.id, focusId: rootId });
      })
      .catch(() => {
        // meiliDocumentFetch only swallows document_not_found; anything here
        // is a real fetch failure, so say so rather than claiming the post
        // isn't indexed.
        if (!cancelled) setResolveFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [rootDoc.data?.type, rootId, onResolveRoot]);

  const { comments, result, isError, isWalking } = useThread(rootId);
  const tree = useMemo(() => buildTree(rootId, comments), [rootId, comments]);

  // Scroll to the comment that was searched for, once it has actually loaded.
  // Re-runs as levels arrive because a deep comment appears late in the walk.
  const focused = useRef(false);
  useEffect(() => {
    focused.current = false;
  }, [rootId, focusId]);
  useEffect(() => {
    if (!focusId || focused.current) return;
    const el = document.getElementById(`c-${focusId}`);
    if (!el) return;
    focused.current = true;
    el.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [focusId, comments.length]);

  const toggle = (id: number) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  const root = rootDoc.data;

  // isSuccess, not isFetched: a thrown query also counts as fetched, and
  // "not in the index" would then be a lie about a Meilisearch failure.
  if (rootDoc.isError) {
    return (
      <Notice title="Couldn't load this thread">
        <p>Meilisearch didn't answer for item #{rootId}.</p>
        <div className="mt-3 flex gap-4">
          <button onClick={onClose} className="hover:text-primary">
            ← back to results
          </button>
          <a
            href={hnItemUrl(rootId)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary"
          >
            view on HN ↗
          </a>
        </div>
      </Notice>
    );
  }

  if (rootDoc.isSuccess && !root) {
    return (
      <Notice title="That item isn't in the index">
        <p>
          Item #{rootId} was never indexed, or was deleted on Hacker News.
        </p>
        <div className="mt-3 flex gap-4">
          <button onClick={onClose} className="hover:text-primary">
            ← back to results
          </button>
          <a
            href={hnItemUrl(rootId)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary"
          >
            view on HN ↗
          </a>
        </div>
      </Notice>
    );
  }

  const expected = root?.num_comments ?? 0;
  const loaded = comments.length;

  return (
    <div>
      <div className="flex items-baseline justify-between border-b pb-2 font-mono text-xs text-muted-foreground">
        <button
          onClick={onClose}
          className="flex items-center gap-1 hover:text-primary"
        >
          <ArrowLeft className="size-3" /> back to results
        </button>
        <span className="tabular-nums">
          {isWalking
            ? `${loaded}${expected > loaded ? ` / ${expected}` : ""} loading…`
            : `${loaded.toLocaleString("en-US")} ${loaded === 1 ? "comment" : "comments"}`}
        </span>
      </div>

      {root ? <RootHeader root={root} /> : <RootSkeleton />}

      {root?.type === "comment" && !resolveFailed && (
        <p className="mt-2 border border-accent-foreground/25 bg-accent px-2 py-1 font-mono text-[11px] text-accent-foreground">
          This thread's original post isn't in the index — showing the
          discussion from the highest comment we could reach.
        </p>
      )}

      <div className="mt-3">
        {tree.length === 0 && !isWalking && !isError && (
          <p className="py-6 font-mono text-sm text-muted-foreground">
            No comments yet.
          </p>
        )}

        {tree.map((node) => (
          <CommentNode
            key={node.id}
            node={node}
            collapsed={collapsed}
            focusId={focusId}
            onToggle={toggle}
          />
        ))}

        {isWalking && (
          <div className="flex flex-col gap-2 py-4">
            <Skeleton className="h-3 w-3/5" />
            <Skeleton className="h-3 w-4/5" />
          </div>
        )}
      </div>

      {(isError || result?.error || resolveFailed) && (
        <p className="mt-3 flex items-center gap-2 border p-3 font-mono text-xs text-muted-foreground">
          <TriangleAlert className="size-3.5 shrink-0" />
          Couldn't load deeper replies.
          <a
            href={hnItemUrl(rootId)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary"
          >
            read the rest on HN ↗
          </a>
        </p>
      )}

      {result && !isWalking && (
        <div className="mt-4 flex flex-wrap items-baseline justify-between gap-2 border-t pt-2 font-mono text-[11px] text-muted-foreground">
          <span className="tabular-nums">
            {loaded.toLocaleString("en-US")} comments · {result.depth}{" "}
            {result.depth === 1 ? "level" : "levels"} deep · {result.elapsedMs} ms
            {result.truncated && " · capped"}
          </span>
          <a
            href={hnItemUrl(rootId)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary"
          >
            view on HN ↗
          </a>
        </div>
      )}
    </div>
  );
}

function RootHeader({ root }: { root: HNHit }) {
  const timeAgo = root.created_at
    ? formatDistanceToNowStrict(new Date(root.created_at * 1000), { addSuffix: true })
    : "";

  return (
    <header className="border-b-2 py-3">
      <h2 className="text-[15px] leading-snug font-medium">
        <a
          href={root.url ?? hnItemUrl(root.id)}
          target="_blank"
          rel="noreferrer"
          className="hover:text-primary"
        >
          {root.title ?? `item #${root.id}`}
          <ArrowUpRight className="ml-0.5 inline size-3.5 align-baseline text-muted-foreground" />
        </a>
        {root.domain && (
          <span className="ml-2 align-baseline font-mono text-xs font-normal text-muted-foreground">
            ({root.domain})
          </span>
        )}
      </h2>
      {root.text && (
        <p className="mt-2 text-sm leading-relaxed text-foreground/85 [overflow-wrap:anywhere]">
          {root.text}
        </p>
      )}
      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 font-mono text-xs text-muted-foreground">
        {root.type !== "comment" && (
          <span className="tabular-nums">
            <span className="text-primary">▲</span> {root.points}
          </span>
        )}
        <span>
          by{" "}
          <a
            href={hnUserUrl(root.author)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary hover:underline"
          >
            {root.author}
          </a>
        </span>
        <span>{timeAgo}</span>
      </div>
    </header>
  );
}

function RootSkeleton() {
  return (
    <div className="flex flex-col gap-2 border-b-2 py-3">
      <Skeleton className="h-4 w-4/5" />
      <Skeleton className="h-3 w-2/5" />
    </div>
  );
}

function Notice({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="mt-6 border bg-card p-6 font-mono text-sm text-muted-foreground">
      <h2 className="mb-2 font-semibold text-foreground">{title}</h2>
      {children}
    </div>
  );
}
```

- [ ] **Step 2: Verify it compiles and lints**

Run: `cd web && pnpm exec tsc --noEmit && pnpm lint`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add web/src/app/thread-view.tsx
git commit -m "Add the thread view with progressive loading and error states"
```

---

### Task 8: History plumbing and rendering the thread

Wire thread state into `SearchApp` so `?item=<id>` renders a thread and the back button works. Entry points come in Task 9; this task is verified via a deep link.

**Files:**
- Modify: `web/src/app/search-app.tsx`

**Interfaces:**
- Consumes: `ThreadView` (Task 7); `ThreadState`, `threadFromParams`, `threadToParams` (Task 4).
- Produces: `openThread(thread: ThreadState, seed?: HNHit): void` and `closeThread(): void`, both passed down in Task 9.

- [ ] **Step 1: Add the imports**

In `web/src/app/search-app.tsx`, add to the existing `@/lib/search-state` import: `threadFromParams`, `threadToParams`, and `type ThreadState`. Add `type HNHit` to the existing `@/lib/meili` import. Add alongside the `Results` import:

```tsx
import { ThreadView } from "./thread-view";
```

- [ ] **Step 2: Add thread state**

Immediately after the existing `const [state, setState] = useState<SearchState>(…)` declaration:

```tsx
const [thread, setThread] = useState<ThreadState | null>(() =>
  threadFromParams(new URLSearchParams(searchParams.toString())),
);
// The already-loaded hit behind an open thread, so its header paints without
// a fetch. Absent on deep links and after a back/forward navigation.
const [threadSeed, setThreadSeed] = useState<HNHit | undefined>(undefined);
// Opening or closing a thread is a navigation and pushes; every other state
// change keeps rewriting the current entry, as the app has always done.
const historyMode = useRef<"replace" | "push">("replace");
```

- [ ] **Step 3: Close the thread on any search interaction**

Replace the existing `update` callback:

```tsx
// Every filter change resets pagination; explicit page changes override.
// Touching any search control also leaves an open thread — no dead controls.
const update = useCallback((patch: Partial<SearchState>) => {
  setState((prev) => ({ ...prev, page: 1, ...patch }));
  setThread(null);
  setThreadSeed(undefined);
}, []);
```

And add the same two lines to the end of the `setScope` callback's body, after its `setState`:

```tsx
    setThread(null);
    setThreadSeed(undefined);
```

- [ ] **Step 4: Replace the URL effect**

Replace the existing effect that calls `window.history.replaceState`:

```tsx
useEffect(() => {
  const params = threadToParams(thread, stateToParams(state));
  const qs = params.toString();
  const url = qs ? `?${qs}` : window.location.pathname;
  if (historyMode.current === "push") window.history.pushState(null, "", url);
  else window.history.replaceState(null, "", url);
  historyMode.current = "replace";
}, [state, thread]);
```

- [ ] **Step 5: Handle back and forward**

Add a new effect below it:

```tsx
// The app wrote history with replaceState only, so back did nothing. Now that
// threads push entries, both kinds of state have to be rehydrated from the URL.
useEffect(() => {
  const onPopState = () => {
    const params = new URLSearchParams(window.location.search);
    setState(paramsToState(params));
    setThread(threadFromParams(params));
    setThreadSeed(undefined);
  };
  window.addEventListener("popstate", onPopState);
  return () => window.removeEventListener("popstate", onPopState);
}, []);
```

- [ ] **Step 6: Add the open/close callbacks**

Add below the `resetFilters` declaration:

```tsx
const openThread = useCallback((next: ThreadState, seed?: HNHit) => {
  historyMode.current = "push";
  setThread(next);
  setThreadSeed(seed);
  window.scrollTo({ top: 0 });
}, []);

const closeThread = useCallback(() => {
  historyMode.current = "push";
  setThread(null);
  setThreadSeed(undefined);
}, []);

// A deep link can point at a comment; ThreadView resolves it upward and the
// URL is corrected in place rather than pushing a second history entry.
const replaceThread = useCallback((next: ThreadState) => {
  setThread(next);
  setThreadSeed(undefined);
}, []);
```

- [ ] **Step 7: Render the thread instead of the results**

Replace the `<Results … />` element inside `<section className="min-w-0 flex-1">`:

```tsx
{thread ? (
  <ThreadView
    rootId={thread.rootId}
    focusId={thread.focusId}
    seed={threadSeed}
    onClose={closeThread}
    onResolveRoot={replaceThread}
  />
) : (
  <Results
    search={search}
    state={state}
    onPage={(page) => {
      update({ page });
      window.scrollTo({ top: 0 });
    }}
    onPrefetchPage={prefetchPage}
    onState={update}
  />
)}
```

- [ ] **Step 8: Verify it compiles and lints**

Run: `cd web && pnpm exec tsc --noEmit && pnpm lint`
Expected: no errors.

- [ ] **Step 9: Verify the deep link in the browser**

Start the preview with `preview_start` (`{name: "web"}`, creating `.claude/launch.json` if absent — `pnpm dev` in `web/`, port 3000). This needs a reachable Meilisearch with data; if the configured host is down, start a local one and index a slice:

```bash
docker compose up -d meilisearch && cd indexer && cargo run --release -- backfill --recent 50000
```

Then navigate to `http://localhost:3000/?item=<a story id with comments>` and confirm: the thread renders, the back button returns to the search results, and `read_console_messages` reports no errors.

- [ ] **Step 10: Commit**

```bash
git add web/src/app/search-app.tsx
git commit -m "Render threads from the URL and make the back button work"
```

---

### Task 9: Entry points from the result cards

Turn the comment count and the "thread" link into in-app actions.

**Files:**
- Modify: `web/src/app/hit-card.tsx:114-146`
- Modify: `web/src/app/results.tsx:13-19` and `:101-111`
- Modify: `web/src/app/search-app.tsx` (pass `openThread` to `Results`)

**Interfaces:**
- Consumes: `openThread` (Task 8); `ThreadState` (Task 4).
- Produces: an `onOpenThread: (thread: ThreadState, seed?: HNHit) => void` prop threaded `SearchApp → Results → HitCard`.

- [ ] **Step 1: Add the prop to `HitCard`**

In `web/src/app/hit-card.tsx`, extend the imports with `type ThreadState` from `@/lib/search-state`, and add to `HitCardProps`:

```tsx
  onOpenThread: (thread: ThreadState, seed?: HNHit) => void;
```

Add `onOpenThread` to the destructured parameters of the component.

- [ ] **Step 2: Replace the outbound comments link**

Replace the trailing `<a>` in the metadata row (currently the `hnItemUrl` link containing `MessageSquare`) with:

```tsx
        <button
          onClick={() =>
            isComment
              ? // A comment's own id is a valid starting point: ThreadView
                // resolves it upward and corrects the URL to the real root.
                onOpenThread({ rootId: hit.id, focusId: hit.id })
              : onOpenThread({ rootId: hit.id }, hit)
          }
          className="inline-flex items-center gap-1 hover:text-primary hover:underline"
        >
          <MessageSquare className="size-3" />
          {isComment ? "thread" : `${hit.num_comments} comments`}
        </button>
        <a
          href={hnItemUrl(hit.id)}
          target="_blank"
          rel="noreferrer"
          className="hover:text-primary"
          title="View on Hacker News"
        >
          HN ↗
        </a>
```

- [ ] **Step 3: Thread the prop through `Results`**

In `web/src/app/results.tsx`, add to `ResultsProps`:

```tsx
  onOpenThread: (thread: ThreadState, seed?: HNHit) => void;
```

Import `type ThreadState` from `@/lib/search-state` and `type HNHit` from `@/lib/meili`, add `onOpenThread` to the destructured parameters, and pass it to `<HitCard>`:

```tsx
          <HitCard
            key={hit.id}
            hit={hit}
            domains={state.domains}
            authors={state.authors}
            onState={onState}
            onOpenThread={onOpenThread}
          />
```

- [ ] **Step 4: Pass it from `SearchApp`**

Add `onOpenThread={openThread}` to the `<Results …>` element in `web/src/app/search-app.tsx`.

- [ ] **Step 5: Verify it compiles and lints**

Run: `cd web && pnpm exec tsc --noEmit && pnpm lint`
Expected: no errors.

- [ ] **Step 6: Run the full test suite**

Run: `cd web && pnpm test`
Expected: PASS — 34 tests.

- [ ] **Step 7: Verify the whole feature in the browser**

With the preview running, confirm each of these:

1. On the News tab, clicking "N comments" on a story opens the nested thread; the header appears immediately (seeded), and comments fill in.
2. `[−]` collapses a subtree and shows "N replies hidden"; `[+]` restores it.
3. Deep subthreads stop indenting rather than shrinking to one word per line — check at both desktop and mobile widths via `resize_window`.
4. "back to results" returns to the same search, unchanged.
5. The browser back button does the same.
6. On the Comments tab, clicking "thread" on a comment opens the parent discussion, scrolls to that comment, and highlights it; the URL becomes `?…&item=<story>&c=<comment>`.
7. Typing in the search box while a thread is open closes it and searches.
8. `read_console_messages` reports no errors throughout.

- [ ] **Step 8: Commit**

```bash
git add web/src/app/hit-card.tsx web/src/app/results.tsx web/src/app/search-app.tsx
git commit -m "Open threads from story and comment cards"
```

---

## Verification

After Task 9, the whole feature is in place. Final check before review:

```bash
cd web && pnpm test && pnpm exec tsc --noEmit && pnpm lint && pnpm build
```

All four must pass. `pnpm build` catches Next.js-specific problems — server/client boundary violations in particular — that `tsc` alone does not.
