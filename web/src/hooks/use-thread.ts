"use client";

import { useQuery } from "@tanstack/react-query";
import { useState } from "react";

import type { HNHit } from "@/lib/meili";
import { fetchThread, type ThreadResult } from "@/lib/thread";

/**
 * Run the downward walk for one thread.
 *
 * Levels are accumulated into state as they arrive so the top of a thread is
 * readable long before a deep walk finishes; the resolved query data takes
 * over as the source of truth once the walk completes.
 */
export function useThread(rootId: number | null) {
  const [levels, setLevels] = useState<HNHit[]>([]);
  // Reset accumulated levels during render when the thread changes, so a stale
  // thread is never briefly shown under a new root. This is React's documented
  // "adjust state when props change" pattern, not an effect.
  const [trackedRoot, setTrackedRoot] = useState(rootId);
  if (trackedRoot !== rootId) {
    setTrackedRoot(rootId);
    setLevels([]);
  }

  const query = useQuery<ThreadResult>({
    queryKey: ["hn-thread", rootId],
    enabled: rootId !== null,
    // Threads are effectively immutable for a browsing session; re-walking on
    // every remount would be pure waste.
    staleTime: 5 * 60_000,
    queryFn: ({ signal }) => {
      // `enabled` already guarantees this, but narrowing beats casting.
      if (rootId === null) throw new Error("useThread: no thread is open");
      return fetchThread(rootId, {
        signal,
        onLevel: (level) => setLevels((prev) => [...prev, ...level]),
      });
    },
  });

  return {
    comments: query.data?.comments ?? levels,
    result: query.data,
    isPending: query.isPending && rootId !== null,
    isError: query.isError,
    isWalking: query.isFetching,
  };
}
