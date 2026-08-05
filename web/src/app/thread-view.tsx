"use client";

import { useQuery } from "@tanstack/react-query";
import { formatDistanceToNowStrict } from "date-fns";
import { ArrowLeft, ArrowUpRight, TriangleAlert } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";

import { Skeleton } from "@/components/ui/skeleton";
import { hnItemUrl, hnUserUrl, type HNHit } from "@/lib/meili";
import type { ThreadState } from "@/lib/search-state";
import { buildTree, meiliDocumentFetch, resolveThreadRoot } from "@/lib/thread";
import { useThread } from "@/hooks/use-thread";

import { CommentNode } from "./comment-node";

interface ThreadViewProps {
  rootId: number;
  focusId?: number;
  /** The already-loaded hit that was clicked, when there was one. An
   *  optimisation only — a deep link arrives without it. */
  seed?: HNHit;
  onClose: () => void;
  onResolveRoot: (thread: ThreadState) => void;
}

export function ThreadView({
  rootId,
  focusId,
  seed,
  onClose,
  onResolveRoot,
}: ThreadViewProps) {
  const [collapsed, setCollapsed] = useState<Set<number>>(new Set());

  // A failed upward resolve is NOT a broken ancestor chain, and must not be
  // reported as one — see the banner condition below.
  const [resolveFailed, setResolveFailed] = useState(false);

  // Reset collapsed state and resolve-failure state when the thread changes,
  // during render rather than in an effect — same "adjust state when props
  // change" pattern as useThread's `trackedRoot` — so a stale thread's
  // collapsed set or failure banner is never briefly shown under a new root.
  const [trackedRootId, setTrackedRootId] = useState(rootId);
  if (trackedRootId !== rootId) {
    setTrackedRootId(rootId);
    setCollapsed(new Set());
    setResolveFailed(false);
  }

  const rootDoc = useQuery({
    queryKey: ["hn-item", rootId],
    queryFn: () => meiliDocumentFetch(rootId),
    initialData: seed,
    staleTime: 5 * 60_000,
  });

  // A deep link can point at a comment. Resolve it upward and hand the real
  // root back so the URL and the walk both move to the top of the thread.
  useEffect(() => {
    if (rootDoc.data?.type !== "comment") return;
    let cancelled = false;
    resolveThreadRoot(rootId)
      .then((resolved) => {
        if (cancelled || !resolved || resolved.root.id === rootId) return;
        onResolveRoot({ rootId: resolved.root.id, focusId: rootId });
      })
      .catch(() => {
        // meiliDocumentFetch only swallows document_not_found; anything here
        // is a real fetch failure, so say so rather than claiming the post
        // isn't indexed.
        if (!cancelled) setResolveFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [rootDoc.data?.type, rootId, onResolveRoot]);

  const { comments, result, isError, isWalking } = useThread(rootId);
  const tree = useMemo(() => buildTree(rootId, comments), [rootId, comments]);

  // Scroll to the comment that was searched for, once it has actually loaded.
  // Re-runs as levels arrive because a deep comment appears late in the walk.
  const focused = useRef(false);
  useEffect(() => {
    focused.current = false;
  }, [rootId, focusId]);
  useEffect(() => {
    if (!focusId || focused.current) return;
    const el = document.getElementById(`c-${focusId}`);
    if (!el) return;
    focused.current = true;
    el.scrollIntoView({ block: "center", behavior: "smooth" });
    // comments.length is a deliberate dependency: a deep comment appears late
    // in the walk, so this effect keeps retrying until the anchor exists.
  }, [focusId, comments.length]);

  const toggle = (id: number) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  const root = rootDoc.data;

  // isSuccess, not isFetched: a thrown query also counts as fetched, and
  // "not in the index" would then be a lie about a Meilisearch failure.
  if (rootDoc.isError) {
    return (
      <Notice title="Couldn't load this thread">
        <p>Meilisearch didn&apos;t answer for item #{rootId}.</p>
        <div className="mt-3 flex gap-4">
          <button onClick={onClose} className="hover:text-primary">
            ← back to results
          </button>
          <a
            href={hnItemUrl(rootId)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary"
          >
            view on HN ↗
          </a>
        </div>
      </Notice>
    );
  }

  if (rootDoc.isSuccess && !root) {
    return (
      <Notice title="That item isn't in the index">
        <p>
          Item #{rootId} was never indexed, or was deleted on Hacker News.
        </p>
        <div className="mt-3 flex gap-4">
          <button onClick={onClose} className="hover:text-primary">
            ← back to results
          </button>
          <a
            href={hnItemUrl(rootId)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary"
          >
            view on HN ↗
          </a>
        </div>
      </Notice>
    );
  }

  const expected = root?.num_comments ?? 0;
  const loaded = comments.length;

  return (
    <div>
      <div className="flex items-baseline justify-between border-b pb-2 font-mono text-xs text-muted-foreground">
        <button
          onClick={onClose}
          className="flex items-center gap-1 hover:text-primary"
        >
          <ArrowLeft className="size-3" /> back to results
        </button>
        <span className="tabular-nums">
          {isWalking
            ? `${loaded}${expected > loaded ? ` / ${expected}` : ""} loading…`
            : `${loaded.toLocaleString("en-US")} ${loaded === 1 ? "comment" : "comments"}`}
        </span>
      </div>

      {root ? <RootHeader root={root} /> : <RootSkeleton />}

      {root?.type === "comment" && !resolveFailed && (
        <p className="mt-2 border border-accent-foreground/25 bg-accent px-2 py-1 font-mono text-[11px] text-accent-foreground">
          This thread&apos;s original post isn&apos;t in the index — showing
          the discussion from the highest comment we could reach.
        </p>
      )}

      <div className="mt-3">
        {tree.length === 0 && !isWalking && !isError && (
          <p className="py-6 font-mono text-sm text-muted-foreground">
            No comments yet.
          </p>
        )}

        {tree.map((node) => (
          <CommentNode
            key={node.id}
            node={node}
            collapsed={collapsed}
            focusId={focusId}
            onToggle={toggle}
          />
        ))}

        {isWalking && (
          <div className="flex flex-col gap-2 py-4">
            <Skeleton className="h-3 w-3/5" />
            <Skeleton className="h-3 w-4/5" />
          </div>
        )}
      </div>

      {(isError || result?.error || resolveFailed) && (
        <p className="mt-3 flex items-center gap-2 border p-3 font-mono text-xs text-muted-foreground">
          <TriangleAlert className="size-3.5 shrink-0" />
          Couldn&apos;t load deeper replies.
          <a
            href={hnItemUrl(rootId)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary"
          >
            read the rest on HN ↗
          </a>
        </p>
      )}

      {result && !isWalking && (
        <div className="mt-4 flex flex-wrap items-baseline justify-between gap-2 border-t pt-2 font-mono text-[11px] text-muted-foreground">
          <span className="tabular-nums">
            {loaded.toLocaleString("en-US")} comments · {result.depth}{" "}
            {result.depth === 1 ? "level" : "levels"} deep · {result.elapsedMs} ms
            {result.truncated && " · capped"}
          </span>
          <a
            href={hnItemUrl(rootId)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary"
          >
            view on HN ↗
          </a>
        </div>
      )}
    </div>
  );
}

function RootHeader({ root }: { root: HNHit }) {
  const timeAgo = root.created_at
    ? formatDistanceToNowStrict(new Date(root.created_at * 1000), { addSuffix: true })
    : "";

  return (
    <header className="border-b-2 py-3">
      <h2 className="text-[15px] leading-snug font-medium">
        <a
          href={root.url ?? hnItemUrl(root.id)}
          target="_blank"
          rel="noreferrer"
          className="hover:text-primary"
        >
          {root.title ?? `item #${root.id}`}
          <ArrowUpRight className="ml-0.5 inline size-3.5 align-baseline text-muted-foreground" />
        </a>
        {root.domain && (
          <span className="ml-2 align-baseline font-mono text-xs font-normal text-muted-foreground">
            ({root.domain})
          </span>
        )}
      </h2>
      {root.text && (
        <p className="mt-2 text-sm leading-relaxed text-foreground/85 [overflow-wrap:anywhere]">
          {root.text}
        </p>
      )}
      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 font-mono text-xs text-muted-foreground">
        {root.type !== "comment" && (
          <span className="tabular-nums">
            <span className="text-primary">▲</span> {root.points}
          </span>
        )}
        <span>
          by{" "}
          <a
            href={hnUserUrl(root.author)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary hover:underline"
          >
            {root.author}
          </a>
        </span>
        <span>{timeAgo}</span>
      </div>
    </header>
  );
}

function RootSkeleton() {
  return (
    <div className="flex flex-col gap-2 border-b-2 py-3">
      <Skeleton className="h-4 w-4/5" />
      <Skeleton className="h-3 w-2/5" />
    </div>
  );
}

function Notice({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="mt-6 border bg-card p-6 font-mono text-sm text-muted-foreground">
      <h2 className="mb-2 font-semibold text-foreground">{title}</h2>
      {children}
    </div>
  );
}
