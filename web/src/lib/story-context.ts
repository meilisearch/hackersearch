import { COMMENTS_INDEX, STORIES_INDEX, meili, type HNHit } from "./meili";
import { MAX_ANCESTOR_HOPS } from "./thread";

/** The item a comment ultimately replies to: a story, job or poll. */
export interface StoryRef {
  id: number;
  title: string;
}

/** Just enough of an item to keep walking up, or to name the story. */
type Ancestor = Pick<HNHit, "id" | "type" | "parent" | "title">;

/** Reads several items by id, in whichever index each one lives. */
export type BatchFetch = (ids: number[]) => Promise<Ancestor[]>;

const ANCESTOR_FIELDS: (keyof Ancestor)[] = ["id", "type", "parent", "title"];

/**
 * HN ids are unique across types, so asking both indexes for the same id
 * list returns each item exactly once — one parallel round-trip per level.
 */
export const meiliBatchFetch: BatchFetch = async (ids) => {
  const [comments, stories] = await Promise.all(
    [COMMENTS_INDEX, STORIES_INDEX].map((uid) =>
      meili.index(uid).getDocuments<Ancestor>({
        ids,
        // A no-op filter makes the client POST to /documents/fetch: one
        // fixed URL, so the browser's CORS preflight is cached (24h) instead
        // of repeated for every round's distinct `?ids=` GET URL.
        filter: [],
        fields: ANCESTOR_FIELDS,
        limit: ids.length,
      }),
    ),
  );
  return [...comments.results, ...stories.results];
};

/**
 * Items already read this session. Ancestors are shared heavily — a page of
 * comments from one busy thread climbs through the same few parents — and an
 * item's type, parent and title effectively never change.
 */
const cache = new Map<number, Ancestor | null>();
const CACHE_LIMIT = 20_000;

/**
 * Name the story behind each comment hit.
 *
 * Comments only carry `parent`, so this climbs the reply chain — but for all
 * hits at once: each round batches every still-unknown ancestor into ONE
 * lookup, so a page costs as many round-trips as its deepest comment is
 * deep, not one per comment per level. A hit whose chain breaks (a deleted
 * or never-indexed ancestor) or runs past the hop cap is left out.
 */
export async function resolveStories(
  hits: Pick<HNHit, "id" | "parent">[],
  fetchBatch: BatchFetch = meiliBatchFetch,
  /** Called with everything resolved so far after each round, so shallow
   *  replies can show their story before the deepest chain finishes. */
  onProgress?: (stories: Map<number, StoryRef>) => void,
): Promise<Map<number, StoryRef>> {
  if (cache.size > CACHE_LIMIT) cache.clear();

  // Each hit's current position on its way up; null once settled.
  const cursor = new Map<number, number | null>(
    hits.map((hit) => [hit.id, hit.parent ?? null]),
  );
  const stories = new Map<number, StoryRef>();

  for (let hop = 0; hop <= MAX_ANCESTOR_HOPS; hop += 1) {
    // Climb every hit as far as the cache already allows.
    for (const [hitId, at] of cursor) {
      let position = at;
      // Bounded so a malformed parent cycle among cached items terminates.
      let steps = 0;
      while (position !== null && cache.has(position)) {
        const item = cache.get(position);
        if (!item || steps > MAX_ANCESTOR_HOPS) {
          position = null; // broken chain
        } else if (item.type !== "comment") {
          if (item.title) stories.set(hitId, { id: item.id, title: item.title });
          position = null;
        } else {
          position = item.parent ?? null;
        }
        steps += 1;
      }
      cursor.set(hitId, position);
    }

    // Every non-null cursor now sits on an id the cache has never seen.
    const missing = [
      ...new Set([...cursor.values()].filter((id): id is number => id !== null)),
    ];
    if (missing.length === 0 || hop === MAX_ANCESTOR_HOPS) break;
    if (hop > 0) onProgress?.(new Map(stories));

    const found = await fetchBatch(missing);
    for (const id of missing) cache.set(id, null);
    for (const item of found) cache.set(item.id, item);
  }

  return stories;
}

/** Forget everything read so far — for tests. */
export function clearStoryCache() {
  cache.clear();
}
