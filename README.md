# HackerSearch

A full replacement for HN Search: **all of Hacker News indexed in
[Meilisearch](https://www.meilisearch.com)** — every story, every comment —
behind a unified, faceted search UI.

```
┌─────────────┐   Firebase API    ┌────────────┐   REST    ┌─────────────┐
│ Hacker News │ ────────────────▶ │ hn-indexer │ ────────▶ │ Meilisearch │
└─────────────┘  backfill + sync  │   (Rust)   │           │  index: hn  │
                                  └────────────┘           └──────┬──────┘
                                                        multi-search│
                                                           ┌───────▼──────┐
                                                           │  web (Next)  │
                                                           │ faceted UI   │
                                                           └──────────────┘
```

## Quick start

```sh
docker compose watch
```

Then open <http://localhost:3000>. On first boot the indexer applies the index
settings, backfills the most recent 100,000 items (~2 minutes), and then follows
new/updated items live every 30 seconds.

## Indexing the full corpus

The complete corpus is ~45M items. At the indexer's default concurrency
(~750 items/s) a full backfill takes **≈17 hours** and is fully resumable — kill
it any time and it picks up from its checkpoint.

```sh
# via compose (persists its checkpoint in the indexer_state volume):
BACKFILL_RECENT= docker compose up -d indexer

# or directly on the host:
cd indexer
MEILI_URL=http://localhost:7701 \
MEILI_MASTER_KEY=hackersearch-dev-master-key \
cargo run --release -- backfill
```

## The indexer

```
hn-indexer settings                    # create index + apply search settings
hn-indexer backfill [--recent N]       # index maxitem → 1 (or just last N ids)
hn-indexer backfill --since-days 30    # index everything posted in the last N days
hn-indexer sync [--interval 30]        # follow new + updated items forever
hn-indexer run [--recent N] [--enrich] # settings + backfill + sync (+ enrich), one process
hn-indexer enrich                      # crawl story URLs, store extracted article text
hn-indexer enrich --since 2025-01-01   # …only stories posted on or after a date
hn-indexer enrich --watch              # …and keep going, picking up new stories
hn-indexer settings --embedder huggingface|openai   # enable semantic search
```

| Env var | Default | Purpose |
|---|---|---|
| `MEILI_URL` | `http://localhost:7700` | Meilisearch endpoint |
| `MEILI_MASTER_KEY` | — | API key |
| `INDEXER_CONCURRENCY` | `128` | Parallel HN API requests |
| `INDEXER_BATCH_SIZE` | `2000` | Ids per chunk / docs per payload |
| `INDEXER_STATE_FILE` | `indexer-state.json` | Resume checkpoint |
| `BACKFILL_RECENT` | — | Limit `run` backfill depth (unset = full corpus) |
| `SYNC_INTERVAL` | `30` | Live-sync poll seconds |
| `ENRICH_ON_SYNC` | — | Enrich continuously inside `run` (compose sets `true`) |
| `ENRICH_SINCE` | — | Only enrich stories from this date on (compose: `2025-01-01`) |
| `ENRICH_MAX_CHARS` | `4000` | Characters of article text kept per document |
| `ENRICH_EXTRACTOR` | `auto` | `auto` \| `local` \| `cloudflare` |
| `ENRICH_INTERVAL` | `60` | `--watch` poll seconds |
| `CLOUDFLARE_ACCOUNT_ID` | — | Browser Rendering account (enables `auto`) |
| `CLOUDFLARE_API_TOKEN` | — | Browser Rendering token (enables `auto`) |

Deleted and dead items are skipped. Comment HTML is stripped at index time, so
documents are plain text and the UI never renders HTML from HN.

## Article enrichment & semantic search

Inspired by [hackerverse](https://github.com/wilsonzlin/hackerverse):
`hn-indexer enrich` fetches the page each story links to, strips
semantically non-primary HTML (`nav`, `header`, `footer`, `aside`, scripts…),
and stores the main article text on the document as `content`. That field is
**not full-text indexed** — it exists to feed embeddings, so semantic search
understands what an article is *about* rather than only its title.

```sh
hn-indexer enrich                        # crawl + extract (resumable, ~23 docs/s)
hn-indexer enrich --since 2025-01-01     # scope to recent stories
hn-indexer enrich --watch                # stay up, enriching newly indexed stories
hn-indexer settings --embedder huggingface   # local ONNX model, no API key
OPENAI_API_KEY=… hn-indexer settings --embedder openai  # text-embedding-3-small
```

`content` is **tri-state**, so a page that can never be extracted is
distinguishable from one the crawler simply hasn't reached:

| `content` | Meaning |
|---|---|
| absent | never attempted |
| `""` | attempted, nothing extractable (paywall, PDF, dead link…) |
| non-empty | article text |

"Never attempted" is *absent* rather than an explicit `null` on purpose:
`add_documents` upserts with PUT, and a literal `content: null` in the sync
payload would wipe crawled text every time HN bumps a story's score.

Each processed document is stamped with `enrich_gen` (the current
`ENRICH_GENERATION` in `meili.rs`). `enrich` selects anything stamped with an
older generation — or nothing at all — so **bumping that constant re-crawls
the corpus** under the new extractor without any flag or manual flag-clearing.
Documents drop out of the selection as they're stamped, which is what makes
the pass resumable and idempotent.

### Continuous enrichment

`--watch` keeps the loop alive after the backlog drains, re-polling every
`ENRICH_INTERVAL` seconds so stories indexed later get article text too.
`hn-indexer run --enrich` does the same thing inside the long-running service,
next to backfill and sync — which is how compose runs it (`ENRICH_ON_SYNC`).

Pair it with `ENRICH_SINCE`. Crawling every story URL in the corpus is a
~48-hour job at Cloudflare's capped concurrency; compose defaults to
`2025-01-01` so the window stays bounded. Widen it by setting an earlier date.

Two extractors are available (`--extractor auto|local|cloudflare`):

- **local** — plain HTTP fetch + readability-style extraction with the
  `scraper` crate (strips nav/header/footer/aside/scripts, prefers
  `<article>`/`<main>`). No dependencies, ~83% success rate.
- **cloudflare** — [Browser Rendering's markdown endpoint](https://developers.cloudflare.com/browser-run/quick-actions/markdown-endpoint/):
  a real headless browser renders the page (JS-heavy sites included) and
  returns markdown, which is cleaned of link targets/images before storage.
  Needs `CLOUDFLARE_ACCOUNT_ID` + `CLOUDFLARE_API_TOKEN`; concurrency is
  capped at 6 and 429s are retried with backoff. Any per-page failure falls
  back to the local extractor. `auto` (the default) uses Cloudflare exactly
  when the credentials are present.

The embedder's document template prefers `title + content`, falling back to
the item's own text (comments). Once embeddings exist, the UI shows a
**✦ semantic** toggle (set `NEXT_PUBLIC_MEILISEARCH_EMBEDDER=default`) that
blends keyword and vector results (`semanticRatio: 0.6`). Run `enrich` before
enabling the embedder so documents aren't embedded twice.

## The index

Documents (`id` primary key):
`type`, `tags` (`story`, `comment`, `ask_hn`, `show_hn`, `launch_hn`, `job`, …),
`title`, `text`, `url`, `domain`, `author`, `points`, `num_comments`,
`created_at` (unix seconds), `parent`, plus `content` and `enrich_gen` written
by `enrich`.

- **Searchable**: title, text, url, domain, author
- **Facets/filters**: tags, type, author, domain, points, num_comments,
  created_at, enrich_gen
- **Sorts**: relevance (with a `points:desc` tiebreaker), newest, points

## The web UI

Next.js App Router + Tailwind v4 + shadcn/ui + TanStack Query. One Meilisearch
`multi-search` round-trip per keystroke: the main paginated query plus one
facet-count query per dimension (with that dimension's own filter excluded),
which gives correct **disjunctive facet counts**. Two top-level tabs partition
the corpus — **News** (stories, Ask/Show/Launch HN, jobs, polls) and
**Comments** — with the facet rail adapting to each. Search state lives in the
URL, so every search is a shareable link. Press `/` to focus the search box.

Host-side dev (Meilisearch still via Docker):

```sh
docker compose up -d meilisearch
cd web && pnpm install && pnpm dev
```

`web/.env.local` points the browser at Meilisearch (`http://localhost:7701` by
default). **The dev master key is exposed to the browser for local convenience —
use a search-scoped API key in any real deployment.**

Note: the host port is **7701** (not 7700) to avoid colliding with other local
Meilisearch instances.
