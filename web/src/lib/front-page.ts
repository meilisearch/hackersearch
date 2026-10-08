import { searchHN, type HNSearchResult } from "./meili";
import { DEFAULT_STATE, hasActiveFilters, type SearchRequest } from "./search-state";

/** Where the shared front-page result is served from (app/api/front-page). */
export const FRONT_PAGE_URL = "/api/front-page";

/**
 * The search every visitor runs on arrival: no query, no filters, News tab,
 * relevance, page 1. It ranks all ~5M stories by points and counts facets
 * over all of them — 0.7–0.9 s of engine time, identical for everyone — so
 * it is computed once on the server and cached at the edge instead.
 *
 * Both value facets are computed, so the one cached response serves any
 * combination of open rail sections.
 */
export const FRONT_PAGE_REQUEST: SearchRequest = {
  ...DEFAULT_STATE,
  openFacets: ["domain", "author"],
};

/** Whether `s` is the front-page search, whatever rail sections are open. */
export function isFrontPage(s: SearchRequest): boolean {
  return (
    s.scope === "news" &&
    s.q === "" &&
    s.sort === "relevance" &&
    s.page === 1 &&
    !hasActiveFilters(s)
  );
}

/**
 * Run a search, answering the front-page one from the shared cache. Any
 * failure there (route down, bad response) falls back to a live search, so
 * the cache can only ever make the front page faster, never break it.
 */
export async function searchOrFrontPage(
  s: SearchRequest,
  signal?: AbortSignal,
): Promise<HNSearchResult> {
  if (isFrontPage(s)) {
    const startedAt = performance.now();
    try {
      const res = await fetch(FRONT_PAGE_URL, { signal });
      if (res.ok) {
        const result = (await res.json()) as HNSearchResult;
        return {
          ...result,
          cached: true,
          roundTripMs: Math.round(performance.now() - startedAt),
        };
      }
    } catch (error) {
      if (signal?.aborted) throw error;
    }
  }
  return searchHN(s, signal);
}
