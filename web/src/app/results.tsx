"use client";

import type { UseQueryResult } from "@tanstack/react-query";
import { ChevronLeft, ChevronRight, ServerCrash } from "lucide-react";

import { Skeleton } from "@/components/ui/skeleton";
import { MEILI_HOST, type HNSearchResult } from "@/lib/meili";
import type { SearchState } from "@/lib/search-state";
import { cn } from "@/lib/utils";

import { HitCard } from "./hit-card";

interface ResultsProps {
  search: UseQueryResult<HNSearchResult, Error>;
  state: SearchState;
  indexEmpty: boolean;
  onPage: (page: number) => void;
  onState: (patch: Partial<SearchState>) => void;
}

export function Results({ search, state, indexEmpty, onPage, onState }: ResultsProps) {
  const { data, isPending, isError, isFetching } = search;

  if (isError) {
    return (
      <Notice icon={<ServerCrash className="size-5" />} title="Meilisearch unreachable">
        <p>
          No Meilisearch instance answered at{" "}
          <code className="text-accent-foreground">{MEILI_HOST}</code>.
        </p>
        <pre className="mt-3 border bg-muted p-3">docker compose up -d meilisearch</pre>
      </Notice>
    );
  }

  if (isPending) {
    return (
      <div className="flex flex-col gap-6 pt-2">
        {Array.from({ length: 8 }, (_, i) => (
          <div key={i} className="flex flex-col gap-2">
            <Skeleton className="h-4 w-4/5" />
            <Skeleton className="h-3 w-2/5" />
          </div>
        ))}
      </div>
    );
  }

  if (data.totalHits === 0 && indexEmpty) {
    return (
      <Notice title="The index is empty">
        <p>Start the indexer to pull Hacker News into Meilisearch:</p>
        <pre className="mt-3 border bg-muted p-3">
          {"docker compose up -d\n# or, for the full 40M+ item corpus:\ncd indexer && cargo run --release -- backfill"}
        </pre>
      </Notice>
    );
  }

  if (data.totalHits === 0) {
    return (
      <Notice title="No results">
        <p>
          Nothing matches{state.q ? <> “{state.q}”</> : null} with the current
          filters. Try widening the time range or removing facets.
        </p>
      </Notice>
    );
  }

  return (
    <div>
      <div
        className={cn(
          "flex items-baseline justify-between border-b pb-2 font-mono text-xs text-muted-foreground transition-opacity",
          isFetching && "opacity-60",
        )}
      >
        <span className="tabular-nums">
          {data.totalHits.toLocaleString("en-US")}
          {data.totalHits === 10_000 ? "+" : ""} results ·{" "}
          {data.processingTimeMs} ms
        </span>
        <span className="tabular-nums">
          page {data.page}/{data.totalPages.toLocaleString("en-US")}
        </span>
      </div>

      <div className={cn("transition-opacity", isFetching && "opacity-60")}>
        {data.hits.map((hit) => (
          <HitCard key={hit.id} hit={hit} state={state} onState={onState} />
        ))}
      </div>

      <Pagination page={data.page} totalPages={data.totalPages} onPage={onPage} />
    </div>
  );
}

function Notice({
  title,
  icon,
  children,
}: {
  title: string;
  icon?: React.ReactNode;
  children?: React.ReactNode;
}) {
  return (
    <div className="mt-6 border bg-card p-6 font-mono text-sm text-muted-foreground">
      <h2 className="mb-2 flex items-center gap-2 font-semibold text-foreground">
        {icon}
        {title}
      </h2>
      {children}
    </div>
  );
}

function Pagination({
  page,
  totalPages,
  onPage,
}: {
  page: number;
  totalPages: number;
  onPage: (page: number) => void;
}) {
  if (totalPages <= 1) return null;

  const windowPages = [page - 2, page - 1, page, page + 1, page + 2].filter(
    (p) => p >= 1 && p <= totalPages,
  );

  return (
    <nav className="flex items-center justify-center gap-1 py-6 font-mono text-xs">
      <PageButton disabled={page === 1} onClick={() => onPage(page - 1)}>
        <ChevronLeft className="size-3.5" />
      </PageButton>
      {windowPages[0] > 1 && (
        <>
          <PageButton onClick={() => onPage(1)}>1</PageButton>
          {windowPages[0] > 2 && <span className="px-1 text-muted-foreground">…</span>}
        </>
      )}
      {windowPages.map((p) => (
        <PageButton key={p} active={p === page} onClick={() => onPage(p)}>
          {p}
        </PageButton>
      ))}
      {windowPages[windowPages.length - 1] < totalPages && (
        <>
          {windowPages[windowPages.length - 1] < totalPages - 1 && (
            <span className="px-1 text-muted-foreground">…</span>
          )}
          <PageButton onClick={() => onPage(totalPages)}>
            {totalPages}
          </PageButton>
        </>
      )}
      <PageButton disabled={page === totalPages} onClick={() => onPage(page + 1)}>
        <ChevronRight className="size-3.5" />
      </PageButton>
    </nav>
  );
}

function PageButton({
  children,
  onClick,
  active,
  disabled,
}: {
  children: React.ReactNode;
  onClick: () => void;
  active?: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className={cn(
        // min-w keeps 1-2 digit pages square; longer numbers grow via padding.
        "flex h-7 min-w-7 items-center justify-center border px-1.5 tabular-nums transition-colors",
        active
          ? "border-primary bg-primary text-primary-foreground"
          : "bg-card text-muted-foreground hover:border-primary hover:text-primary",
        disabled && "pointer-events-none opacity-40",
      )}
    >
      {children}
    </button>
  );
}
