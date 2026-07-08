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
  processingTimeMs: number;
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

/**
 * One round-trip: the main paginated query plus one facet-count query per
 * dimension with that dimension's own filter removed, so checking a value
 * never zeroes out its siblings (disjunctive faceting).
 */
export async function searchHN(s: SearchState): Promise<HNSearchResult> {
  const dimensions: FilterDimension[] = ["tags", "domain", "author"];
  // Hybrid (keyword + vector) applies to the main query only; facet counts
  // stay keyword-based. Meaningless without a query or under explicit sorts.
  const hybrid =
    s.semantic && EMBEDDER && s.q && s.sort === "relevance"
      ? { hybrid: { embedder: EMBEDDER, semanticRatio: 0.6 } }
      : {};
  const { results } = await meili.multiSearch({
    queries: [
      {
        indexUid: INDEX_UID,
        q: s.q,
        ...hybrid,
        filter: buildFilter(s),
        sort: SORTS[s.sort],
        hitsPerPage: HITS_PER_PAGE,
        page: s.page,
        attributesToHighlight: ["title", "text"],
        highlightPreTag: HL_START,
        highlightPostTag: HL_END,
        attributesToCrop: ["text"],
        cropLength: 45,
      },
      ...dimensions.map((dim) => ({
        indexUid: INDEX_UID,
        q: s.q,
        filter: buildFilter(s, dim),
        facets: [dim],
        limit: 0,
      })),
      {
        indexUid: INDEX_UID,
        q: s.q,
        filter: buildFilter(s),
        sort: ["points:desc"],
        limit: 5,
        attributesToRetrieve: ["id", "title", "text"],
      },
    ],
  });

  const main = results[0];
  const facetFor = (i: number, dim: FilterDimension): FacetCounts =>
    (results[i + 1]?.facetDistribution?.[dim] as FacetCounts | undefined) ?? {};

  return {
    hits: (main.hits as HNHit[]) ?? [],
    completionHits: (results[4]?.hits as HNHit[]) ?? [],
    totalHits:
      "totalHits" in main ? (main.totalHits as number) : main.hits.length,
    totalPages: "totalPages" in main ? (main.totalPages as number) : 1,
    page: s.page,
    processingTimeMs: main.processingTimeMs,
    facets: {
      tags: facetFor(0, "tags"),
      domain: facetFor(1, "domain"),
      author: facetFor(2, "author"),
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
): Promise<FacetValueHit[]> {
  const res = await meili.index(INDEX_UID).searchForFacetValues({
    facetName: dim,
    facetQuery,
    q: s.q,
    filter: buildFilter(s, dim),
  });
  // facet-search has no `limit` param — it's bounded only by the index's
  // faceting.maxValuesPerFacet (100), so truncate client-side instead.
  return res.facetHits.slice(0, MAX_FACET_ROWS);
}

export interface IndexStats {
  numberOfDocuments: number;
  isIndexing: boolean;
}

export async function fetchIndexStats(): Promise<IndexStats> {
  const stats = await meili.index(INDEX_UID).getStats();
  return {
    numberOfDocuments: stats.numberOfDocuments,
    isIndexing: stats.isIndexing,
  };
}

export function hnItemUrl(id: number): string {
  return `https://news.ycombinator.com/item?id=${id}`;
}

export function hnUserUrl(author: string): string {
  return `https://news.ycombinator.com/user?id=${encodeURIComponent(author)}`;
}
