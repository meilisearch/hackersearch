"use client";

import { formatDistanceToNowStrict } from "date-fns";
import { Minus, Plus } from "lucide-react";
import { memo } from "react";

import { hnItemUrl, hnUserUrl } from "@/lib/meili";
import { countDescendants, type ThreadNode } from "@/lib/thread";
import { cn } from "@/lib/utils";

interface CommentNodeProps {
  node: ThreadNode;
  collapsed: Set<number>;
  focusId?: number;
  onToggle: (id: number) => void;
}

export const CommentNode = memo(function CommentNodeInner({
  node,
  collapsed,
  focusId,
  onToggle,
}: CommentNodeProps) {
  const isCollapsed = collapsed.has(node.id);
  const isFocus = node.id === focusId;
  const hidden = isCollapsed ? countDescendants(node) : 0;
  const timeAgo = node.created_at
    ? formatDistanceToNowStrict(new Date(node.created_at * 1000), { addSuffix: true })
    : "";

  return (
    <article
      id={`c-${node.id}`}
      className={cn(
        "border-l-2 border-l-border py-1.5 pl-2 transition-colors hover:border-l-primary sm:pl-3",
        // One step of indent per level, dropped past the cap so deep
        // subthreads don't squeeze into a one-word column. Mobile caps
        // earlier than desktop because there is far less room.
        node.depth > 0 && node.depth <= 6 && "ml-3",
        node.depth > 0 && node.depth <= 8 && "sm:ml-5",
        isFocus && "border-l-primary bg-primary/10",
      )}
    >
      <div className="flex flex-wrap items-center gap-x-2 font-mono text-[11px] text-muted-foreground">
        <button
          onClick={() => onToggle(node.id)}
          className="grid size-3.5 place-items-center border text-muted-foreground hover:border-primary hover:text-primary"
          aria-expanded={!isCollapsed}
          aria-label={isCollapsed ? "Expand replies" : "Collapse replies"}
        >
          {isCollapsed ? <Plus className="size-2.5" /> : <Minus className="size-2.5" />}
        </button>
        <a
          href={hnUserUrl(node.author)}
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:underline"
        >
          {node.author}
        </a>
        <span>{timeAgo}</span>
        <a
          href={hnItemUrl(node.id)}
          target="_blank"
          rel="noreferrer"
          className="hover:text-primary"
          title="View this comment on Hacker News"
        >
          ↗
        </a>
        {isCollapsed && hidden > 0 && (
          <span className="opacity-70">
            · {hidden} {hidden === 1 ? "reply" : "replies"} hidden
          </span>
        )}
      </div>

      {!isCollapsed && (
        <>
          {node.text ? (
            <p className="mt-1 text-sm leading-relaxed text-foreground/85 [overflow-wrap:anywhere]">
              {node.text}
            </p>
          ) : (
            <p className="mt-1 font-mono text-xs text-muted-foreground italic">
              [no content]
            </p>
          )}
          {node.children.map((child) => (
            <CommentNode
              key={child.id}
              node={child}
              collapsed={collapsed}
              focusId={focusId}
              onToggle={onToggle}
            />
          ))}
        </>
      )}
    </article>
  );
});

// The inner function is deliberately named differently so recursion goes
// through the memo wrapper; restore the useful name for React DevTools.
CommentNode.displayName = "CommentNode";
