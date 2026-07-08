"use client";

import { formatDistanceToNowStrict } from "date-fns";
import { ArrowUpRight, MessageSquare, TriangleAlert } from "lucide-react";

import { hnItemUrl, type HNHit } from "@/lib/meili";
import type { SearchState } from "@/lib/search-state";
import { cn } from "@/lib/utils";

import { Highlighted } from "./highlighted";

const TAG_LABELS: Record<string, string> = {
  ask_hn: "Ask HN",
  show_hn: "Show HN",
  launch_hn: "Launch HN",
  tell_hn: "Tell HN",
  job: "Job",
  poll: "Poll",
  pollopt: "Poll option",
  comment: "Comment",
};

interface HitCardProps {
  hit: HNHit;
  onState: (patch: Partial<SearchState>) => void;
  state: SearchState;
}

export function HitCard({ hit, onState, state }: HitCardProps) {
  const isComment = hit.type === "comment";
  const timeAgo = hit.created_at
    ? formatDistanceToNowStrict(new Date(hit.created_at * 1000), {
        addSuffix: true,
      })
    : "";
  const specialTag = hit.tags.find((t) => t !== hit.type);
  // The News/Comments tabs already communicate comment-ness; only chip the
  // more specific kinds (Ask HN, Show HN, jobs, polls, …).
  const chip =
    specialTag ??
    (hit.type !== "story" && hit.type !== "comment" ? hit.type : undefined);

  return (
    <article
      className={cn(
        "group border-b py-4 first:pt-1",
        isComment && "border-l-2 border-l-border pl-4 transition-colors hover:border-l-primary",
      )}
    >
      {chip && (
        <span className="mb-1 inline-block border border-accent-foreground/25 bg-accent px-1.5 py-px font-mono text-[10px] text-accent-foreground">
          {TAG_LABELS[chip] ?? chip}
        </span>
      )}

      {hit.title && (
        <h2 className="text-[15px] leading-snug font-medium">
          <a
            href={hit.url ?? hnItemUrl(hit.id)}
            target="_blank"
            rel="noreferrer"
            className="hover:text-primary"
          >
            <Highlighted text={hit._formatted?.title ?? hit.title} />
            <ArrowUpRight className="ml-0.5 inline size-3.5 align-baseline text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />
          </a>
          {hit.domain && (
            <button
              onClick={() =>
                onState({
                  domains: state.domains.includes(hit.domain!)
                    ? state.domains
                    : [...state.domains, hit.domain!],
                })
              }
              className="ml-2 align-baseline font-mono text-xs font-normal text-muted-foreground hover:text-primary hover:underline"
              title={`Filter by ${hit.domain}`}
            >
              ({hit.domain})
            </button>
          )}
        </h2>
      )}

      {hit.text && (
        <p
          className={cn(
            "mt-1 text-sm leading-relaxed text-foreground/85",
            !isComment && "text-foreground/70",
          )}
        >
          <Highlighted text={hit._formatted?.text ?? hit.text} />
        </p>
      )}

      {!hit.title && !hit.text && (
        <p className="mt-1 flex items-center gap-1.5 font-mono text-xs text-muted-foreground">
          <TriangleAlert className="size-3.5" /> item #{hit.id} has no content
        </p>
      )}

      <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-xs text-muted-foreground">
        {!isComment && (
          <span className="tabular-nums">
            <span className="text-primary">▲</span> {hit.points}
          </span>
        )}
        <span>
          by{" "}
          <button
            onClick={() =>
              onState({
                authors: state.authors.includes(hit.author)
                  ? state.authors
                  : [...state.authors, hit.author],
              })
            }
            className="hover:text-primary hover:underline"
            title={`Filter by ${hit.author}`}
          >
            {hit.author}
          </button>
        </span>
        <span className="hidden sm:inline">{timeAgo}</span>
        <a
          href={hnItemUrl(hit.id)}
          target="_blank"
          rel="noreferrer"
          className="inline-flex items-center gap-1 hover:text-primary"
        >
          <MessageSquare className="size-3" />
          {isComment ? "thread" : `${hit.num_comments} comments`}
        </a>
      </div>
    </article>
  );
}
