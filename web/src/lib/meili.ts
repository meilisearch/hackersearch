import { Meilisearch } from "meilisearch";

import { DATE_RANGES, type SearchState } from "./search-state";

export const MEILI_HOST =
  process.env.NEXT_PUBLIC_MEILISEARCH_HOST ?? "http://localhost:7700";
export const MEILI_KEY = process.env.NEXT_PUBLIC_MEILISEARCH_API_KEY ?? "";
export const INDEX_UID = "hn";
// Name of the configured Meilisearch embedder; empty = semantic search off.
export const EMBEDDER = process.env.NEXT_PUBLIC_MEILISEARCH_EMBEDDER ?? "";

export const HITS_PER_PAGE = 20;

// Private-use-area markers survive JSON round-trips and can never appear in
// real HN content, so highlighted fields can be rendered without innerHTML.
export const HL_START = "\u{E000}";
export const HL_END = "\u{E001}";

export const meili = new Meilisearch({
  host: MEILI_HOST,
  apiKey: MEILI_KEY || undefined,
});

export interface HNDoc {
  id: number;
  type: "story" | "comment" | "job" | "poll" | "pollopt";
  tags: string[];
  title?: string;
  text?: string;
  url?: string;
  domain?: string;
  author: string;
  points: number;
  num_comments: number;
  created_at: number;
  parent?: number;
}

export interface HNHit extends HNDoc {
  _formatted?: {
    title?: string;
    text?: string;
    url?: string;
  };
}

export type FacetCounts = Record<string, number>;

export interface HNSearchResult {
  hits: HNHit[];
  /** Top hits by points for the same query — the source for the inline
   *  ghost completion, so it always proposes the most popular match. */
  completionHits: HNHit[];
  totalHits: number;
  totalPages: number;
  page: number;
  /** Engine-side time reported by Meilisearch for the main query. */
  processingTimeMs: number;
  /** Full client-observed round-trip for the multi-search request. */
  roundTripMs: number;
  facets: {
    tags: FacetCounts;
    domain: FacetCounts;
    author: FacetCounts;
  };
}

type FilterDimension = "tags" | "domain" | "author";

const quote = (value: string) => JSON.stringify(value);

/** Build a Meilisearch filter: inner arrays are ORed, outer entries ANDed. */
function buildFilter(
  s: SearchState,
  exclude?: FilterDimension,
): (string | string[])[] {
  const filter: (string | string[])[] = [];
  // The News/Comments tabs partition the corpus before any facet applies.
  filter.push(
    s.scope === "comments" ? 'type = "comment"' : 'type != "comment"',
  );
  if (exclude !== "tags" && s.tags.length) {
    filter.push(s.tags.map((t) => `tags = ${quote(t)}`));
  }
  if (exclude !== "domain" && s.domains.length) {
    filter.push(s.domains.map((d) => `domain = ${quote(d)}`));
  }
  if (exclude !== "author" && s.authors.length) {
    filter.push(s.authors.map((a) => `author = ${quote(a)}`));
  }
  const range = DATE_RANGES.find((d) => d.value === s.dateRange);
  if (range && range.seconds > 0) {
    const cutoff = Math.floor(Date.now() / 1000) - range.seconds;
    filter.push(`created_at >= ${cutoff}`);
  }
  if (s.minPoints > 0) {
    filter.push(`points >= ${s.minPoints}`);
  }
  return filter;
}

const SORTS: Record<SearchState["sort"], string[] | undefined> = {
  relevance: undefined,
  date: ["created_at:desc"],
  points: ["points:desc"],
};

const DIMENSION_SELECTION: Record<FilterDimension, (s: SearchState) => number> = {
  tags: (s) => s.tags.length,
  domain: (s) => s.domains.length,
  author: (s) => s.authors.length,
};

// Ask Meilisearch for its per-step timing breakdown on every request.
// Servers older than ~1.48 reject unknown search parameters, so the first
// "Unknown field" response flips this off and the request is retried plain.
let performanceDetails = true;
const perfParam = () => (performanceDetails ? { showPerformanceDetails: true } : {});
const isUnknownPerfParam = (error: unknown) =>
  error instanceof Error && error.message.includes("showPerformanceDetails");

/**
 * One round-trip batching, at most:
 *   1. the main paginated query (also carrying every facet's counts);
 *   2. one facet-count query per dimension that has an ACTIVE selection,
 *      with that dimension's own filter removed (disjunctive faceting);
 *   3. a points-sorted query feeding the inline ghost completion.
 *
 * A dimension without a selection reads its counts straight off the main
 * query, and (2)/(3) are omitted when not needed — so the common "typing a
 * fresh query, no facets checked" case sends 1 query instead of 5.
 */
export async function searchHN(
  s: SearchState,
  signal?: AbortSignal,
): Promise<HNSearchResult> {
  const dimensions: FilterDimension[] = ["tags", "domain", "author"];
  const startedAt = performance.now();
  // Hybrid (keyword + vector) applies to the main query only; facet counts
  // stay keyword-based. Meaningless without a query or under explicit sorts.
  const hybrid =
    s.semantic && EMBEDDER && s.q && s.sort === "relevance"
      ? { hybrid: { embedder: EMBEDDER, semanticRatio: 0.6 } }
      : {};

  // Only dimensions with an active selection need their own exclusion query;
  // for the rest the main query's own distribution is already correct.
  const activeDims = dimensions.filter((d) => DIMENSION_SELECTION[d](s) > 0);

  // Skip the completion query unless a ghost suffix could actually render
  // (mirrors findCompletion's rules + the 40-char cap in search-app).
  const lastWord = s.q.split(/\s+/).pop() ?? "";
  const wantCompletion = s.q.length <= 40 && lastWord.length >= 2;

  const buildQueries = () => [
    {
      indexUid: INDEX_UID,
      q: s.q,
      ...hybrid,
      ...perfParam(),
      filter: buildFilter(s),
      sort: SORTS[s.sort],
      hitsPerPage: HITS_PER_PAGE,
      page: s.page,
      facets: dimensions,
      attributesToHighlight: ["title", "text"],
      highlightPreTag: HL_START,
      highlightPostTag: HL_END,
      attributesToCrop: ["text"],
      cropLength: 45,
    },
    ...activeDims.map((dim) => ({
      indexUid: INDEX_UID,
      q: s.q,
      ...perfParam(),
      filter: buildFilter(s, dim),
      facets: [dim],
      limit: 0,
    })),
    ...(wantCompletion
      ? [
          {
            indexUid: INDEX_UID,
            q: s.q,
            ...perfParam(),
            // Complete only from story titles: match the title field alone,
            // restrict to posts (comments/poll options have no title), and
            // take just the single highest-pointed match — the ghost only
            // ever uses the top one.
            attributesToSearchOn: ["title"],
            filter: 'type != "comment"',
            sort: ["points:desc"],
            limit: 1,
            attributesToRetrieve: ["title"],
          },
        ]
      : []),
  ];

  // The signal comes from TanStack Query: superseded searches (new
  // keystroke, changed filter) abort their in-flight HTTP request.
  let results;
  try {
    ({ results } = await meili.multiSearch({ queries: buildQueries() }, { signal }));
  } catch (error) {
    if (performanceDetails && isUnknownPerfParam(error)) {
      performanceDetails = false; // older server; drop the param and retry
      ({ results } = await meili.multiSearch({ queries: buildQueries() }, { signal }));
    } else {
      throw error;
    }
  }

  const main = results[0];
  const mainFacets = (main.facetDistribution ?? {}) as Record<string, FacetCounts>;
  const excluded = new Map<FilterDimension, FacetCounts>();
  activeDims.forEach((dim, i) => {
    excluded.set(
      dim,
      (results[1 + i]?.facetDistribution?.[dim] as FacetCounts | undefined) ?? {},
    );
  });
  const facetFor = (dim: FilterDimension): FacetCounts =>
    excluded.get(dim) ?? mainFacets[dim] ?? {};

  const completionHits = wantCompletion
    ? ((results[1 + activeDims.length]?.hits as HNHit[]) ?? [])
    : [];

  return {
    hits: (main.hits as HNHit[]) ?? [],
    completionHits,
    totalHits:
      "totalHits" in main ? (main.totalHits as number) : main.hits.length,
    totalPages: "totalPages" in main ? (main.totalPages as number) : 1,
    page: s.page,
    processingTimeMs: main.processingTimeMs,
    roundTripMs: Math.round(performance.now() - startedAt),
    facets: {
      tags: facetFor("tags"),
      domain: facetFor("domain"),
      author: facetFor("author"),
    },
  };
}

export interface FacetValueHit {
  value: string;
  count: number;
}

// Keeps the domain/author facet lists short enough to never need scrolling.
export const MAX_FACET_ROWS = 10;

/**
 * Search values of one facet via Meilisearch's facet-search endpoint,
 * scoped to the current query and every OTHER dimension's filters — the same
 * disjunctive rule the facet counts follow.
 */
export async function searchFacetValues(
  dim: "domain" | "author",
  facetQuery: string,
  s: SearchState,
  signal?: AbortSignal,
): Promise<FacetValueHit[]> {
  const res = await meili.index(INDEX_UID).searchForFacetValues(
    {
      facetName: dim,
      facetQuery,
      q: s.q,
      ...perfParam(),
      filter: buildFilter(s, dim),
    },
    { signal },
  );
  // facet-search has no `limit` param — it's bounded only by the index's
  // faceting.maxValuesPerFacet (100), so truncate client-side instead.
  return res.facetHits.slice(0, MAX_FACET_ROWS);
}

export function hnItemUrl(id: number): string {
  return `https://news.ycombinator.com/item?id=${id}`;
}

export function hnUserUrl(author: string): string {
  return `https://news.ycombinator.com/user?id=${encodeURIComponent(author)}`;
}
