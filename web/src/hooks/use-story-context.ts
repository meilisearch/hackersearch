"use client";

import { useQuery, useQueryClient } from "@tanstack/react-query";

import type { HNHit } from "@/lib/meili";
import { resolveStories, type StoryRef } from "@/lib/story-context";

const NONE = new Map<number, StoryRef>();

/**
 * The story each comment hit on the page belongs to, keyed by comment id.
 * Resolves after the results paint, so cards show their story line a beat
 * later instead of the whole page waiting on the climb.
 */
export function useStoryContext(hits: HNHit[] | undefined): Map<number, StoryRef> {
  const comments = (hits ?? []).filter(
    (hit) => hit.type === "comment" && hit.parent != null,
  );
  const ids = comments.map((hit) => hit.id);
  const queryKey = ["story-context", ids];
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey,
    enabled: ids.length > 0,
    // An item's story never changes.
    staleTime: Infinity,
    queryFn: () =>
      resolveStories(comments, undefined, (partial) =>
        queryClient.setQueryData(queryKey, partial),
      ),
  });
  return query.data ?? NONE;
}
