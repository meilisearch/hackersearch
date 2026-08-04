"use client";

import { useCallback, useEffect, useState } from "react";

import type { ValueFacetDim } from "@/lib/search-state";

const STORAGE_KEY = "hackersearch:open-facets";

const isDim = (v: unknown): v is ValueFacetDim =>
  v === "domain" || v === "author";

/** Collapsed is the default for anything missing, malformed, or unreadable. */
function read(): ValueFacetDim[] {
  if (typeof window === "undefined") return [];
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter(isDim) : [];
  } catch {
    // Storage disabled (private mode) or a corrupt value — start collapsed.
    return [];
  }
}

/**
 * The user's EXPLICIT expand/collapse preference for the domain and author
 * facets, persisted across sessions.
 *
 * Deliberately narrow: a section is also force-expanded while it has active
 * selections, but that derivation lives in the caller so it never gets written
 * back here — otherwise checking one domain would pin the section open forever.
 *
 * Read synchronously on mount (not in an effect) so the very first search
 * already reflects the preference instead of firing twice. Safe because
 * SearchApp is never server-rendered with content: it reads useSearchParams
 * inside the Suspense boundary in page.tsx, which opts that boundary out of
 * prerendering.
 */
export function useOpenFacets(): [
  ValueFacetDim[],
  (dim: ValueFacetDim) => void,
] {
  const [open, setOpen] = useState<ValueFacetDim[]>(read);

  useEffect(() => {
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(open));
    } catch {
      // Non-fatal: the preference just won't survive this session.
    }
  }, [open]);

  const toggle = useCallback((dim: ValueFacetDim) => {
    setOpen((prev) =>
      prev.includes(dim) ? prev.filter((d) => d !== dim) : [...prev, dim],
    );
  }, []);

  return [open, toggle];
}
