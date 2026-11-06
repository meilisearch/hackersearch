import { linkify } from "@/lib/linkify";
import { cn } from "@/lib/utils";

/**
 * Comment or self-post body for the thread view. The indexer keeps HN's
 * paragraph breaks and `<pre>` blocks as newlines, so whitespace is
 * preserved, and full URLs become links.
 */
export function RichText({ text, className }: { text: string; className?: string }) {
  return (
    <p className={cn("whitespace-pre-wrap [overflow-wrap:anywhere]", className)}>
      {linkify(text).map((segment, i) =>
        segment.kind === "link" ? (
          <a
            key={i}
            href={segment.url}
            target="_blank"
            rel="nofollow noreferrer"
            className="text-primary underline decoration-primary/30 underline-offset-2 hover:decoration-primary"
          >
            {segment.url}
          </a>
        ) : (
          segment.text
        ),
      )}
    </p>
  );
}
