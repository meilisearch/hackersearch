import { MeilisearchApiError } from "meilisearch";

import { INDEX_UID, meili, type HNHit } from "./meili";

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

/** Ancestor lookups before the upward walk gives up. */
export const MAX_ANCESTOR_HOPS = 25;

export type DocumentFetch = (id: number) => Promise<HNHit | null>;

/** True only for a genuine 404 from Meilisearch — not a network or server failure. */
export function isDocumentNotFound(error: unknown): boolean {
  return error instanceof MeilisearchApiError && error.cause?.code === "document_not_found";
}

/**
 * Read one document by id. meilisearch 0.59's `getDocument` takes no
 * `extraRequestInit`, so this cannot be given an AbortSignal — callers rely on
 * TanStack discarding results for keys it no longer observes.
 */
export const meiliDocumentFetch: DocumentFetch = async (id) => {
  try {
    return await meili.index(INDEX_UID).getDocument<HNHit>(id);
  } catch (error) {
    // Only a genuinely absent document means "no such item" — deleted, dead,
    // or never indexed. Anything else (network, 5xx, auth) must propagate, or
    // an outage would be reported to the user as a broken ancestor chain.
    if (isDocumentNotFound(error)) return null;
    throw error;
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
