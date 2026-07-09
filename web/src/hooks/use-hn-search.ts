"use client";

import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";

import { fetchIndexStats, searchHN } from "@/lib/meili";
import type { SearchState } from "@/lib/search-state";

export function useHNSearch(state: SearchState) {
  return useQuery({
    queryKey: ["hn-search", state],
    // TanStack aborts the signal when this query is superseded, cancelling
    // the previous in-flight request instead of letting it complete.
    queryFn: ({ signal }) => searchHN(state, signal),
    placeholderData: keepPreviousData,
  });
}

export function useIndexStats() {
  return useQuery({
    queryKey: ["hn-stats"],
    queryFn: fetchIndexStats,
    refetchInterval: 15_000,
    retry: false,
  });
}

/** Debounce fast-changing values (the query string) before they hit the network. */
export function useDebounced<T>(value: T, delayMs = 150): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delayMs);
    return () => clearTimeout(timer);
  }, [value, delayMs]);
  return debounced;
}
