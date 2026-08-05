# In-app comment thread view

**Date:** 2026-08-05
**Status:** Approved, ready for implementation planning

## Problem

Clicking "412 comments" on a story card leaves the app for
news.ycombinator.com. The Comments tab can find an individual comment by
search, but there is no way to read the discussion it belongs to. HackerSearch
indexes every comment; it should be able to show a thread.

## Goal

Clicking the comment count on a story, or "thread" on a comment, opens a
nested, HN-style thread inside the app: indented reply hierarchy, collapsible
subtrees, shareable URL, working back button.

## Constraint that shapes the design

Documents carry `parent` (the immediate parent item) but no root-story
pointer. HN's Firebase API does not provide one, and the indexer does not
derive one. "Every comment belonging to story N" is therefore not a filter we
can write.

Resolved by walking the reply tree client-side, one query per depth level. No
indexer change and no reindex; the feature ships against the index exactly as
configured today. `parent` is already filterable with equality (so `IN`
works) and `created_at` is already sortable.

The alternative — deriving `story_id` at index time — is cleaner and would
also unlock searching within a thread, but requires walking ancestors in the
indexer and a full reindex to backfill existing documents. Rejected for now;
see *Future work*.

## Architecture

### `web/src/lib/thread.ts` (new)

All thread data access. No React.

```ts
interface ThreadNode extends HNHit { children: ThreadNode[]; depth: number }
```

- **`fetchThread(rootId, signal, onLevel?)`** — the downward walk. The
  frontier starts as `[rootId]`. Each iteration issues one search per frontier
  chunk with `filter: parent IN [...]`, `sort: created_at:asc`, and
  `hitsPerPage: 1000`, paging through a chunk while `page < totalPages`. The
  next frontier is the ids just returned. Invokes `onLevel` per completed
  level so the view can paint progressively.

  Termination, checked between levels: a level returned nothing, depth reached
  25, or the accumulated count reached 2000 (`truncated: true`). A level
  already in flight always completes rather than being cut mid-way, so the cap
  is a floor and the final count may exceed it slightly.

  The frontier is chunked at **500 ids per query**, and a level's chunks are
  issued in parallel, so filter strings stay bounded on wide levels.

  Takes its search function as a parameter defaulting to the real client, so
  the walk is testable without network.

- **`resolveThreadRoot(commentId, signal)`** — the upward walk for comment
  entry points. `getDocument(id)`; if `type === "story"` return it, otherwise
  recurse on `parent`. Capped at 25 hops. Returns the highest reachable
  ancestor when the chain breaks on a missing document, so the return value is
  usually a story but not guaranteed to be one — hence `root`, not `story`,
  throughout.

- **`buildTree(comments)`** — pure. Flat `HNHit[]` → `ThreadNode[]`, grouped
  by `parent`, siblings in `created_at` order, `depth` assigned during
  construction. Must be cycle-safe: a malformed `parent` chain must not hang
  the render.

Document reads use `getDocument`, not search — exact and cheaper.

### `web/src/hooks/use-thread.ts` (new)

TanStack `useQuery` keyed `["hn-thread", rootId]`, `enabled` only while a
thread is open. Accumulates levels into state via `onLevel` for progressive
paint; the resolved query data is authoritative once the walk completes.
Aborts through the query signal, matching `useHNSearch`.

### State and URL

The thread is **not** part of `SearchState`. Adding it there would change the
search query key and refetch results every time a thread opens or closes.

`SearchApp` holds a sibling `thread: { rootId: number; focusId?: number } |
null`, serialized alongside the existing search params as `&item=<id>` and
`&c=<id>`.

This changes one existing behavior. The app currently only calls
`replaceState`, so the back button does nothing. After this change:

- search-state edits keep using `replaceState`, exactly as today;
- opening or closing a thread uses `pushState`;
- a new `popstate` listener rehydrates both search state and thread state
  from the URL.

### Components

- **`web/src/app/thread-view.tsx`** (new) — replaces `<Results>` in the
  results column while a thread is open. Renders the breadcrumb bar, the story
  header (title, domain, points, author, time, and self-text for Ask HN
  posts), and the tree. Owns the collapsed-node `Set<number>` so it survives
  child re-renders.

- **`web/src/app/comment-node.tsx`** (new) — recursive and memoized, the way
  `HitCard` is today. Per node: `[−]` toggle, author, relative time, `↗`
  permalink to HN, body text. Collapsed state renders
  `[+] author · 3h · 12 replies hidden`.

- **`web/src/app/hit-card.tsx`** (modified) — the comment-count link on
  stories and the "thread" link on comments become in-app actions instead of
  outbound links. Both still expose an explicit "view on HN" affordance.

- **`web/src/app/search-app.tsx`** (modified) — thread state, `pushState` /
  `popstate` wiring, and swapping `<Results>` for `<ThreadView>`.

## Interaction

### Entry points

```
story card "412 comments"
  → thread = { rootId }
  → the clicked HNHit is handed to ThreadView as an optional `root` prop, so
    the header paints with no fetch at all
  → fetchThread walks down; levels stream in

comment card "thread"
  → resolveThreadRoot(commentId)       [2-6 hops up]
  → thread = { rootId, focusId: commentId }
  → walk down, then scroll to and highlight focusId, expanding its ancestors

deep link ?item=<id>
  → no `root` prop available; ThreadView fetches it with getDocument
  → if it resolves to a comment, resolveThreadRoot runs and the URL is
    rewritten to the root with c=<id>
```

`ThreadView` therefore treats `root` as an optimisation, never a
precondition — it must render correctly from `rootId` alone.

### Layout

The thread replaces the results column. Header, tabs and facet rail stay in
place.

**No dead controls.** The search box, tabs, facets and sort remain live while
a thread is open; interacting with any of them closes the thread and applies
the change. Cheaper than inert-and-dimmed, and more forgiving than controls
that ignore input.

### Reading surface

- Indent steps 20px, capped at depth 8 (12px and depth 6 on mobile). Past the
  cap nodes keep the left rule but stop stepping right, so deep subthreads
  don't squeeze into a one-word column.
- Author links to the HN user page via the existing `hnUserUrl` helper. It is
  deliberately **not** a facet button here, unlike `HitCard` — the thread is a
  reading surface, and a click there should not silently rewrite the user's
  search.
- Progressive loading: instant header, skeleton rows for level 1, a thin
  progress rule and a running count while deeper levels arrive. The count
  denominator is the story's `num_comments`; because that figure includes
  deleted items the walk cannot reach, the loaded count may stop short of it.
  Render as `312 / 412` while loading and drop to the walk's own total on
  completion, so the two never sit side by side disagreeing.
- Footer on completion: `412 comments · 7 levels deep · 91ms`, where the
  duration is the wall time of the whole walk, measured the way `searchHN`
  already measures `roundTripMs`.

## Error handling

| Case | Behavior |
|---|---|
| `?item=` points at a comment | Resolve upward, rewrite the URL to the root story with `c=<id>`. Any item id works as a deep link. |
| Upward walk hits a missing ancestor | Root the thread at the highest reachable ancestor, note it inline. Same render path, no error state. |
| `?item=` not in the index | "That item isn't in the index", link to HN, link back to results. |
| A level query fails mid-walk | Keep the partial tree; inline "couldn't load deeper replies · retry". Never discard loaded content. |
| Cap hit (2000 comments or depth 25) | Footer note plus HN link. |
| Story with no comments | "No comments yet." |
| Thread closed or navigated away mid-walk | In-flight walk aborts via the query signal. |

## Known limitation

The indexer drops deleted and dead items (`to_doc` returns `None`). A subtree
whose parent was deleted is therefore unreachable by the walk and silently
absent from the thread — estimated low single-digit percent of comments. HN
itself keeps such nodes as `[dead]` placeholders to hold the tree together.

The fix is tombstone documents in the indexer. Out of scope here; recorded in
*Future work*.

## Testing

`web/` has no test framework today — only `lint`. This adds **Vitest**, the
only net-new dependency in the design, covering the pure logic where the real
bugs live:

- `buildTree` — orphans, sibling ordering, depth assignment, cycle safety
- frontier chunking — boundaries at exactly 500 and 501 ids
- `item` / `c` URL round-trip through the existing param helpers
- `fetchThread` against a stubbed search function: termination on an empty
  level, the depth cap, the comment cap, and partial results after a failing
  level

The view itself gets manual verification in the preview browser.

## Out of scope

- Searching or sorting within a thread (needs `story_id`).
- Voting, replying, or any write path.
- Rendering deleted/dead placeholders.
- Nested rendering anywhere other than the thread view; the Comments tab stays
  a flat search surface.

## Future work

**Index-time `story_id`.** Would collapse the walk to a single filtered query,
close the orphaned-subtree gap, and enable search-within-thread. Requires
resolving each comment's root ancestor in the indexer and a full reindex to
backfill existing documents. `fetchThread` is a narrow enough seam that
swapping the implementation would not touch the view.

**Tombstone documents.** Indexing deleted and dead items as stubs would keep
reply trees connected. Self-healing for new content; needs a reindex for
history.
