export const TAG_OPTIONS = [
  { value: "story", label: "Stories" },
  { value: "comment", label: "Comments" },
  { value: "ask_hn", label: "Ask HN" },
  { value: "show_hn", label: "Show HN" },
  { value: "launch_hn", label: "Launch HN" },
  { value: "job", label: "Jobs" },
  { value: "poll", label: "Polls" },
] as const;

export const DATE_RANGES = [
  { value: "all", label: "All time", seconds: 0 },
  { value: "24h", label: "Past 24 hours", seconds: 86_400 },
  { value: "week", label: "Past week", seconds: 7 * 86_400 },
  { value: "month", label: "Past month", seconds: 30 * 86_400 },
  { value: "year", label: "Past year", seconds: 365 * 86_400 },
] as const;

export const POINTS_OPTIONS = [0, 10, 50, 100, 500] as const;

export type DateRange = (typeof DATE_RANGES)[number]["value"];
export type SortKey = "relevance" | "date" | "points";
export type Scope = "news" | "comments";

/** The two high-cardinality facets whose rail sections collapse. */
export type ValueFacetDim = "domain" | "author";

export interface SearchState {
  scope: Scope;
  semantic: boolean;
  q: string;
  tags: string[];
  domains: string[];
  authors: string[];
  dateRange: DateRange;
  minPoints: number;
  sort: SortKey;
  page: number;
}

/**
 * What actually goes to Meilisearch: the shareable state plus which
 * high-cardinality facets to compute counts for. Openness shapes the request
 * but is a local preference rather than shareable state, so it lives here and
 * never reaches stateToParams / paramsToState / hasActiveFilters.
 */
export interface SearchRequest extends SearchState {
  openFacets: ValueFacetDim[];
}

export const DEFAULT_STATE: SearchState = {
  scope: "news",
  semantic: false,
  q: "",
  tags: [],
  domains: [],
  authors: [],
  dateRange: "all",
  minPoints: 0,
  sort: "relevance",
  page: 1,
};

export function hasActiveFilters(s: SearchState): boolean {
  return (
    s.tags.length > 0 ||
    s.domains.length > 0 ||
    s.authors.length > 0 ||
    s.dateRange !== "all" ||
    s.minPoints > 0
  );
}

export function stateToParams(s: SearchState): URLSearchParams {
  const p = new URLSearchParams();
  if (s.scope !== "news") p.set("tab", s.scope);
  if (s.semantic) p.set("sem", "1");
  if (s.q) p.set("q", s.q);
  if (s.tags.length) p.set("tags", s.tags.join(","));
  if (s.domains.length) p.set("domains", s.domains.join(","));
  if (s.authors.length) p.set("authors", s.authors.join(","));
  if (s.dateRange !== "all") p.set("date", s.dateRange);
  if (s.minPoints > 0) p.set("points", String(s.minPoints));
  if (s.sort !== "relevance") p.set("sort", s.sort);
  if (s.page > 1) p.set("page", String(s.page));
  return p;
}

export function paramsToState(p: URLSearchParams): SearchState {
  const list = (key: string) =>
    p.get(key)?.split(",").filter(Boolean) ?? [];
  const date = p.get("date");
  const sort = p.get("sort");
  return {
    scope: p.get("tab") === "comments" ? "comments" : "news",
    semantic: p.get("sem") === "1",
    q: p.get("q") ?? "",
    tags: list("tags"),
    domains: list("domains"),
    authors: list("authors"),
    dateRange: DATE_RANGES.some((d) => d.value === date)
      ? (date as DateRange)
      : "all",
    minPoints: Math.max(0, Number(p.get("points")) || 0),
    sort: sort === "date" || sort === "points" ? sort : "relevance",
    page: Math.max(1, Number(p.get("page")) || 1),
  };
}

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
