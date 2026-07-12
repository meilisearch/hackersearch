"use client";

import {
  MessagesSquare,
  Newspaper,
  Search,
  SlidersHorizontal,
  Sparkles,
  X,
} from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { Input } from "@/components/ui/input";
import { findCompletion } from "@/lib/autocomplete";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { EMBEDDER, searchHN } from "@/lib/meili";
import { useDebounced, useHNSearch } from "@/hooks/use-hn-search";
import {
  hasActiveFilters,
  paramsToState,
  stateToParams,
  type Scope,
  type SearchState,
  type SortKey,
} from "@/lib/search-state";
import { cn } from "@/lib/utils";

import { FacetRail } from "./facet-rail";
import { Results } from "./results";

const SORT_TABS: { value: SortKey; label: string }[] = [
  { value: "relevance", label: "Relevance" },
  { value: "date", label: "Newest" },
  { value: "points", label: "Points" },
];

const SCOPE_TABS = [
  { value: "news", label: "News", icon: Newspaper },
  { value: "comments", label: "Comments", icon: MessagesSquare },
] as const;

export function SearchApp() {
  const searchParams = useSearchParams();
  const [state, setState] = useState<SearchState>(() =>
    paramsToState(new URLSearchParams(searchParams.toString())),
  );
  const inputRef = useRef<HTMLInputElement>(null);

  // Every filter change resets pagination; explicit page changes override.
  const update = useCallback((patch: Partial<SearchState>) => {
    setState((prev) => ({ ...prev, page: 1, ...patch }));
  }, []);

  // Tags and domains only exist on the News side; points-sort is meaningless
  // for comments. Drop whatever can't apply when switching tabs.
  const setScope = useCallback(
    (scope: Scope) => {
      setState((prev) => ({
        ...prev,
        scope,
        page: 1,
        tags: [],
        domains: [],
        sort:
          scope === "comments" && prev.sort === "points"
            ? "relevance"
            : prev.sort,
      }));
    },
    [],
  );

  useEffect(() => {
    const qs = stateToParams(state).toString();
    window.history.replaceState(null, "", qs ? `?${qs}` : window.location.pathname);
  }, [state]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "/" && document.activeElement?.tagName !== "INPUT") {
        e.preventDefault();
        inputRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const debouncedQ = useDebounced(state.q);
  const queryState = useMemo(
    () => ({ ...state, q: debouncedQ }),
    [state, debouncedQ],
  );
  const search = useHNSearch(queryState);

  // Warm a page's results on hover so clicking it paints instantly. The key
  // must match useHNSearch's exactly, so it's built from the same queryState.
  const queryClient = useQueryClient();
  const prefetchPage = useCallback(
    (page: number) => {
      const target = { ...queryState, page };
      queryClient.prefetchQuery({
        queryKey: ["hn-search", target],
        queryFn: ({ signal }) => searchHN(target, signal),
        staleTime: 30_000,
      });
    },
    [queryClient, queryState],
  );

  // Ghost suffix completing the word being typed, accepted with Tab / →.
  // Long queries scroll inside the input and the overlay wouldn't track,
  // so stop suggesting before that point.
  const ghost = useMemo(() => {
    if (state.q.length > 40) return "";
    return findCompletion(state.q, search.data?.completionHits ?? []);
  }, [state.q, search.data]);

  const resetFilters = () =>
    update({
      tags: [],
      domains: [],
      authors: [],
      dateRange: "all",
      minPoints: 0,
    });
  const activeFilterCount =
    state.tags.length +
    state.domains.length +
    state.authors.length +
    (state.dateRange !== "all" ? 1 : 0) +
    (state.minPoints > 0 ? 1 : 0);
  // Rendered twice: in the desktop rail and inside the mobile filter sheet.
  const facetRail = (
    <FacetRail
      state={state}
      facets={search.data?.facets}
      onChange={update}
      onReset={resetFilters}
      showReset={hasActiveFilters(state)}
    />
  );
  const sortTabs = SORT_TABS.filter(
    (tab) => state.scope !== "comments" || tab.value !== "points",
  );

  return (
    <div className="flex min-h-dvh flex-col">
      <header className="border-b bg-card">
        <div className="mx-auto w-full max-w-6xl px-4 pt-5 pb-0 sm:px-6">
          <div className="flex items-baseline justify-between gap-4">
            <div className="flex items-baseline gap-2.5">
              <span className="grid size-7 translate-y-1 place-items-center bg-primary font-mono text-lg font-semibold text-primary-foreground">
                H
              </span>
              <h1 className="text-xl font-semibold tracking-tight">
                HackerSearch
              </h1>
              <span className="hidden font-mono text-xs text-muted-foreground sm:inline">
                every story · every comment
              </span>
            </div>
            <div className="font-mono text-xs text-muted-foreground">
              <a
                href="https://www.meilisearch.com"
                className="hover:text-primary"
                target="_blank"
                rel="noreferrer"
              >
                meilisearch
              </a>
            </div>
          </div>

          <div className="relative mt-4 w-full max-w-xl">
            <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              ref={inputRef}
              value={state.q}
              onChange={(e) => update({ q: e.target.value })}
              onKeyDown={(e) => {
                if (!ghost) return;
                const el = e.currentTarget;
                const caretAtEnd =
                  el.selectionStart === el.value.length &&
                  el.selectionEnd === el.value.length;
                if (
                  (e.key === "Tab" && !e.shiftKey) ||
                  (e.key === "ArrowRight" && caretAtEnd)
                ) {
                  e.preventDefault();
                  update({ q: state.q + ghost });
                }
              }}
              placeholder={
                state.scope === "comments"
                  ? "Search every HN comment…"
                  : "Search every HN story…"
              }
              className="h-11 rounded-none border bg-background pr-16 pl-9 font-mono text-sm shadow-none focus-visible:ring-0 focus-visible:border-primary md:text-sm"
              autoFocus
            />
            {ghost && (
              // Ghost completion overlay: an invisible copy of the query
              // keeps the suffix aligned with the caret (monospace font).
              <div
                aria-hidden
                className="pointer-events-none absolute inset-0 flex items-center overflow-hidden border border-transparent pr-16 pl-9 font-mono text-sm whitespace-pre"
              >
                <span className="invisible">{state.q}</span>
                <span className="text-muted-foreground/70">{ghost}</span>
              </div>
            )}
            {state.q ? (
              <button
                onClick={() => update({ q: "" })}
                className="absolute top-1/2 right-3 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                aria-label="Clear search"
              >
                <X className="size-4" />
              </button>
            ) : (
              <kbd className="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 border bg-muted px-1.5 font-mono text-[10px] text-muted-foreground">
                /
              </kbd>
            )}
          </div>

          <div className="mt-3 flex items-end justify-between gap-2">
            <nav className="flex gap-0">
              {SCOPE_TABS.map((tab) => (
                <button
                  key={tab.value}
                  onClick={() => setScope(tab.value)}
                  className={cn(
                    "flex items-center gap-2 border-x border-t border-transparent px-3 py-2 text-sm font-medium transition-colors sm:px-4",
                    state.scope === tab.value
                      ? "border-border bg-background text-primary"
                      : "text-muted-foreground hover:text-foreground",
                  )}
                >
                  <tab.icon className="size-4 max-sm:hidden" />
                  {tab.label}
                </button>
              ))}
            </nav>

            <div className="flex items-center gap-2 pb-1 sm:pb-0">
              {EMBEDDER && (
                <button
                  onClick={() => update({ semantic: !state.semantic })}
                  title="Blend keyword and vector search (embeds titles + crawled article content)"
                  className={cn(
                    "flex items-center gap-1.5 border px-3 py-1.5 font-mono text-xs transition-colors",
                    state.semantic
                      ? "border-primary bg-primary text-primary-foreground"
                      : "bg-card text-muted-foreground hover:border-primary hover:text-primary",
                  )}
                >
                  <Sparkles className="size-3.5" />
                  <span className="hidden sm:inline">semantic</span>
                </button>
              )}

              {/* Mobile: facets live in a sheet; sort collapses to a select. */}
              <Sheet>
                <SheetTrigger asChild>
                  <button
                    className="relative flex items-center gap-1.5 border bg-card px-2.5 py-1.5 font-mono text-xs text-muted-foreground transition-colors hover:border-primary hover:text-primary md:hidden"
                    aria-label="Open filters"
                  >
                    <SlidersHorizontal className="size-3.5" />
                    <span className="max-sm:sr-only">filters</span>
                    {activeFilterCount > 0 && (
                      <span className="grid size-4 place-items-center bg-primary text-[10px] font-semibold text-primary-foreground">
                        {activeFilterCount}
                      </span>
                    )}
                  </button>
                </SheetTrigger>
                <SheetContent
                  side="left"
                  className="w-72 overflow-y-auto bg-background"
                >
                  <SheetHeader className="pb-0">
                    <SheetTitle className="font-mono text-xs font-semibold tracking-[0.2em] text-muted-foreground uppercase">
                      Filters
                    </SheetTitle>
                  </SheetHeader>
                  <div className="px-4 pb-8">{facetRail}</div>
                </SheetContent>
              </Sheet>

              <label className="sm:hidden">
                <span className="sr-only">Sort results</span>
                <select
                  value={state.sort}
                  onChange={(e) =>
                    update({ sort: e.target.value as SortKey })
                  }
                  className="border bg-card px-2 py-1.5 font-mono text-xs text-muted-foreground"
                >
                  {sortTabs.map((tab) => (
                    <option key={tab.value} value={tab.value}>
                      {tab.label}
                    </option>
                  ))}
                </select>
              </label>

              <nav className="hidden items-end gap-0 self-end sm:flex">
                {sortTabs.map((tab) => (
                  <button
                    key={tab.value}
                    onClick={() => update({ sort: tab.value })}
                    className={cn(
                      "border-x border-t border-transparent px-4 py-2 font-mono text-xs transition-colors",
                      state.sort === tab.value
                        ? "border-border bg-background text-primary"
                        : "text-muted-foreground hover:text-foreground",
                    )}
                  >
                    {tab.label}
                  </button>
                ))}
              </nav>
            </div>
          </div>
        </div>
      </header>

      <main className="mx-auto flex w-full max-w-6xl flex-1 gap-8 px-4 py-6 sm:px-6">
        <aside className="hidden w-56 shrink-0 md:block">
          <div className="sticky top-6">{facetRail}</div>
        </aside>

        <section className="min-w-0 flex-1">
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
        </section>
      </main>

      <footer className="border-t">
        <div className="mx-auto flex w-full max-w-6xl items-center justify-between px-4 py-3 font-mono text-[11px] text-muted-foreground sm:px-6">
          <span>
            data ·{" "}
            <a
              href="https://github.com/HackerNews/API"
              className="hover:text-primary"
              target="_blank"
              rel="noreferrer"
            >
              HN Firebase API
            </a>
          </span>
          <span>
            search ·{" "}
            <a
              href="https://www.meilisearch.com"
              className="hover:text-primary"
              target="_blank"
              rel="noreferrer"
            >
              Meilisearch
            </a>
          </span>
        </div>
      </footer>
    </div>
  );
}
