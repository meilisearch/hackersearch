"use client";

import {
  keepPreviousData,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { useEffect, useState } from "react";

import { searchHN } from "@/lib/meili";
import type { SearchState } from "@/lib/search-state";

export function useHNSearch(state: SearchState) {
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey: ["hn-search", state],
    queryFn: ({ signal }) => searchHN(state, signal),
    placeholderData: keepPreviousData,
  });

  // Each committed search is a NEW query key, and TanStack does not
  // auto-cancel a stale key's in-flight request when the observer moves on —
  // so without this, superseded searches run to completion (and pile up when
  // the server is slow). Keyed on the SERIALIZED state (not the object ref,
  // which changes every keystroke before the debounce settles), the cleanup
  // fires once per committed search and aborts the one we're leaving.
  // keepPreviousData still shows the last good results while the next loads.
  const stateKey = JSON.stringify(state);
  useEffect(() => {
    return () => {
      queryClient.cancelQueries({
        queryKey: ["hn-search", JSON.parse(stateKey)],
        exact: true,
      });
    };
  }, [queryClient, stateKey]);

  return query;
}

/** Debounce fast-changing values (the query string) before they hit the
 *  network. 100ms keeps keystrokes snappy while still collapsing bursts;
 *  superseded requests abort, so a tight interval is cheap. */
export function useDebounced<T>(value: T, delayMs = 100): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(value), delayMs);
    return () => clearTimeout(timer);
  }, [value, delayMs]);
  return debounced;
}
