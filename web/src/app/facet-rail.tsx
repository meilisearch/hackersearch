"use client";

import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { ChevronRight, Search, X } from "lucide-react";
import { useState } from "react";

import { Checkbox } from "@/components/ui/checkbox";
import { useDebounced } from "@/hooks/use-hn-search";
import {
  MAX_FACET_ROWS,
  searchFacetValues,
  type FacetCounts,
  type HNSearchResult,
} from "@/lib/meili";
import {
  DATE_RANGES,
  POINTS_OPTIONS,
  TAG_OPTIONS,
  type SearchState,
  type ValueFacetDim,
} from "@/lib/search-state";
import { cn } from "@/lib/utils";

interface FacetRailProps {
  state: SearchState;
  facets: HNSearchResult["facets"] | undefined;
  /** Dimensions currently expanded — the only ones with computed counts. */
  openFacets: ValueFacetDim[];
  onToggleFacet: (dim: ValueFacetDim) => void;
  onChange: (patch: Partial<SearchState>) => void;
  onReset: () => void;
  showReset: boolean;
}

function toggle(list: string[], value: string): string[] {
  return list.includes(value)
    ? list.filter((v) => v !== value)
    : [...list, value];
}

const HEADING =
  "font-mono text-[10px] font-semibold tracking-[0.2em] text-muted-foreground uppercase";

/**
 * Static by default. Pass `onToggle` to make the heading an expander — used by
 * the domain/author facets, whose counts are only computed while expanded.
 * `locked` keeps a section with active selections open (its checked values
 * have to stay visible); the toggle is then inert but still shows why.
 */
function Section({
  title,
  open,
  locked,
  onToggle,
  children,
}: {
  title: string;
  open?: boolean;
  locked?: boolean;
  onToggle?: () => void;
  children: React.ReactNode;
}) {
  if (!onToggle) {
    return (
      <section className="border-b pb-4">
        <h3 className={cn("mb-2.5", HEADING)}>{title}</h3>
        {children}
      </section>
    );
  }
  return (
    <section className="border-b pb-4">
      <h3>
        <button
          onClick={onToggle}
          disabled={locked}
          aria-expanded={open}
          title={
            locked
              ? `Clear the ${title.toLowerCase()} filter to collapse`
              : open
                ? `Collapse ${title.toLowerCase()} (stops computing its counts)`
                : `Expand ${title.toLowerCase()}`
          }
          className={cn(
            "mb-2.5 flex w-full items-center gap-1 transition-colors",
            HEADING,
            locked ? "cursor-not-allowed" : "hover:text-foreground",
          )}
        >
          <ChevronRight
            className={cn(
              "size-3 shrink-0 transition-transform",
              open && "rotate-90",
              locked && "opacity-40",
            )}
          />
          {title}
        </button>
      </h3>
      {open && children}
    </section>
  );
}

const count = (n: number | undefined) =>
  n === undefined ? "" : n >= 10_000 ? `${Math.round(n / 1000)}k` : n.toLocaleString("en-US");

export function FacetRail({
  state,
  facets,
  openFacets,
  onToggleFacet,
  onChange,
  onReset,
  showReset,
}: FacetRailProps) {
  const isComments = state.scope === "comments";

  // Stickiness is the desktop rail's concern (see search-app.tsx) — this
  // component also renders inside the mobile filter sheet.
  return (
    <div className="flex flex-col gap-4">
      {!isComments && (
      <Section title="Type">
        <ul className="flex flex-col gap-1.5">
          {TAG_OPTIONS.map((opt) => {
            const n = facets?.tags?.[opt.value];
            const checked = state.tags.includes(opt.value);
            if (!checked && !n) return null;
            return (
              <li key={opt.value}>
                <label className="flex cursor-pointer items-center gap-2 text-sm">
                  <Checkbox
                    checked={checked}
                    onCheckedChange={() =>
                      onChange({ tags: toggle(state.tags, opt.value) })
                    }
                  />
                  <span className={cn(checked && "font-medium text-primary")}>
                    {opt.label}
                  </span>
                  <span className="ml-auto font-mono text-xs text-muted-foreground tabular-nums">
                    {count(n)}
                  </span>
                </label>
              </li>
            );
          })}
        </ul>
      </Section>
      )}

      <Section title="Time">
        <ul className="flex flex-col gap-1">
          {DATE_RANGES.map((range) => (
            <li key={range.value}>
              <button
                onClick={() => onChange({ dateRange: range.value })}
                className={cn(
                  "flex w-full items-center gap-2 py-0.5 text-left text-sm transition-colors",
                  state.dateRange === range.value
                    ? "font-medium text-primary"
                    : "text-foreground/80 hover:text-foreground",
                )}
              >
                <span
                  className={cn(
                    "font-mono text-xs",
                    state.dateRange === range.value
                      ? "text-primary"
                      : "text-transparent",
                  )}
                >
                  ▸
                </span>
                {range.label}
              </button>
            </li>
          ))}
        </ul>
      </Section>

      <Section title="Points">
        <div className="flex flex-wrap gap-1.5">
          {POINTS_OPTIONS.map((points) => (
            <button
              key={points}
              onClick={() => onChange({ minPoints: points })}
              className={cn(
                "border px-2 py-0.5 font-mono text-xs transition-colors",
                state.minPoints === points
                  ? "border-primary bg-primary text-primary-foreground"
                  : "bg-card text-muted-foreground hover:border-primary hover:text-primary",
              )}
            >
              {points === 0 ? "any" : `${points}+`}
            </button>
          ))}
        </div>
      </Section>

      {!isComments && (
        <ValueFacet
          title="Domain"
          dim="domain"
          state={state}
          selected={state.domains}
          distribution={facets?.domain}
          open={openFacets.includes("domain")}
          onToggleOpen={() => onToggleFacet("domain")}
          onToggle={(value) =>
            onChange({ domains: toggle(state.domains, value) })
          }
        />
      )}

      <ValueFacet
        title="Author"
        dim="author"
        state={state}
        selected={state.authors}
        distribution={facets?.author}
        open={openFacets.includes("author")}
        onToggleOpen={() => onToggleFacet("author")}
        onToggle={(value) => onChange({ authors: toggle(state.authors, value) })}
      />

      {showReset && (
        <button
          onClick={onReset}
          className="self-start font-mono text-xs text-muted-foreground underline underline-offset-4 hover:text-primary"
        >
          reset all filters
        </button>
      )}
    </div>
  );
}

function ValueFacet({
  title,
  dim,
  state,
  selected,
  distribution,
  open,
  onToggleOpen,
  onToggle,
}: {
  title: string;
  dim: ValueFacetDim;
  state: SearchState;
  selected: string[];
  distribution: FacetCounts | undefined;
  open: boolean;
  onToggleOpen: () => void;
  onToggle: (value: string) => void;
}) {
  const [facetQuery, setFacetQuery] = useState("");
  const debouncedFacetQuery = useDebounced(facetQuery, 150);
  const facetSearch = useQuery({
    queryKey: ["facet-values", dim, debouncedFacetQuery, state],
    queryFn: ({ signal }) =>
      searchFacetValues(dim, debouncedFacetQuery, state, signal),
    // Collapsing hides the search box, but a stale query string would keep this
    // firing — gate on `open` so a closed section costs nothing at all.
    enabled: open && debouncedFacetQuery.length > 0,
    placeholderData: keepPreviousData,
  });

  const searching = open && facetQuery.length > 0;
  const top = Object.entries(distribution ?? {})
    .sort((a, b) => b[1] - a[1])
    .slice(0, MAX_FACET_ROWS);
  // Searching swaps the top list for facet-search hits over ALL values;
  // otherwise pin selected values first so they stay visible even when
  // they'd otherwise fall out of the top list, then fill up to the cap —
  // capped so the rail never needs to scroll.
  const rows: (readonly [string, number | undefined])[] = searching
    ? (facetSearch.data ?? []).map((h) => [h.value, h.count] as const)
    : [
        ...selected
          .filter((v) => !top.some(([name]) => name === v))
          .map((v) => [v, distribution?.[v]] as const),
        ...top,
      ].slice(0, MAX_FACET_ROWS);

  return (
    // The header always renders: it's the only way to expand the section, and
    // hiding it on an empty distribution would make the expander vanish while
    // collapsed — and flicker out during the ~10ms it takes the newly-requested
    // counts to arrive. `locked` because selections force it open.
    <Section
      title={title}
      open={open}
      locked={selected.length > 0}
      onToggle={onToggleOpen}
    >
      <div className="relative mb-2">
        <Search className="pointer-events-none absolute top-1/2 left-2 size-3 -translate-y-1/2 text-muted-foreground" />
        <input
          value={facetQuery}
          onChange={(e) => setFacetQuery(e.target.value)}
          placeholder={`Search ${title.toLowerCase()}s…`}
          className="w-full border bg-card py-1 pr-6 pl-7 font-mono text-xs outline-none placeholder:text-muted-foreground/70 focus:border-primary"
        />
        {searching && (
          <button
            onClick={() => setFacetQuery("")}
            className="absolute top-1/2 right-2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
            aria-label={`Clear ${title.toLowerCase()} search`}
          >
            <X className="size-3" />
          </button>
        )}
      </div>
      {searching && rows.length === 0 && !facetSearch.isPending && (
        <p className="py-0.5 font-mono text-xs text-muted-foreground">
          no matches
        </p>
      )}
      <ul className="flex flex-col gap-1">
        {rows.map(([value, n]) => {
          const active = selected.includes(value);
          return (
            <li key={value}>
              <button
                onClick={() => onToggle(value)}
                className={cn(
                  "group flex w-full items-center gap-1.5 py-0.5 text-left text-sm transition-colors",
                  active
                    ? "font-medium text-primary"
                    : "text-foreground/80 hover:text-foreground",
                )}
                title={value}
              >
                <span className="min-w-0 flex-1 truncate">{value}</span>
                {active ? (
                  <X className="size-3 shrink-0" />
                ) : (
                  <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">
                    {count(n)}
                  </span>
                )}
              </button>
            </li>
          );
        })}
      </ul>
    </Section>
  );
}
