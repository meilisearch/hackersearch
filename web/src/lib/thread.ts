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
