"use client";

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";

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

  const queryClient = useQueryClient();
  // A deep walk is up to 25 sequential round-trips, and TanStack does not
  // auto-cancel a stale key's in-flight fetch when the observer moves to a
  // new key. Without this, switching threads mid-walk leaves the old walk's
  // `onLevel` closures firing into the new thread's freshly-reset `levels`.
  // Cancelling on the way out aborts the in-flight request (the signal is
  // threaded through `fetchThread`), so no more `onLevel` calls land once
  // we've left this root. Unlike `useHNSearch`, `rootId` is already a
  // primitive, so no serialize-the-key dance is needed here.
  useEffect(() => {
    if (rootId === null) return;
    return () => {
      queryClient.cancelQueries({ queryKey: ["hn-thread", rootId], exact: true });
    };
  }, [queryClient, rootId]);

  const query = useQuery<ThreadResult>({
    queryKey: ["hn-thread", rootId],
    enabled: rootId !== null,
    // Threads are effectively immutable for a browsing session; re-walking on
    // every remount would be pure waste.
    staleTime: 5 * 60_000,
    queryFn: ({ signal }) => {
      // `enabled` already guarantees this, but narrowing beats casting.
      if (rootId === null) throw new Error("useThread: no thread is open");
      // A re-walk of the same root (stale refetch, reconnect, future manual
      // invalidate) starts `fetchThread`'s own accumulator from scratch; without
      // clearing `levels` here too, the previous walk's levels would double up
      // under the fresh ones as `onLevel` re-appends from an empty base.
      setLevels([]);
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
    // `fetchThread` resolves rather than rejects on a level failure (the
    // failure travels in `result.error`), so this almost never fires — check
    // `result?.error` for walk failures, not `isError`.
    isError: query.isError,
    isWalking: query.isFetching,
    // Re-runs the whole walk (queryFn clears the accumulator first) — the
    // retry affordance for a walk that failed partway down.
    refetch: query.refetch,
  };
}
