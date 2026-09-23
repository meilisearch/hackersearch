mod enrich;
mod hn;
mod meili;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::meili::Meili;

#[derive(Parser)]
#[command(
    name = "hn-indexer",
    about = "Index all of Hacker News into Meilisearch"
)]
struct Cli {
    /// Meilisearch URL
    #[arg(
        long,
        env = "MEILI_URL",
        default_value = "http://localhost:7700",
        global = true
    )]
    meili_url: String,

    /// Meilisearch API key (master or an admin key)
    #[arg(long, env = "MEILI_MASTER_KEY", global = true)]
    meili_key: Option<String>,

    /// Number of concurrent HN API requests
    #[arg(
        long,
        env = "INDEXER_CONCURRENCY",
        default_value_t = 128,
        global = true
    )]
    concurrency: usize,

    /// Documents per Meilisearch payload / ids per fetch chunk
    #[arg(
        long,
        env = "INDEXER_BATCH_SIZE",
        default_value_t = 2000,
        global = true
    )]
    batch_size: usize,

    /// Path of the checkpoint file used to resume work
    #[arg(
        long,
        env = "INDEXER_STATE_FILE",
        default_value = "indexer-state.json",
        global = true
    )]
    state_file: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the index and apply search settings
    Settings {
        /// Also configure an embedder for semantic search: openai | voyage
        #[arg(long)]
        embedder: Option<String>,
    },
    /// Configure ONLY the embedder (openai | voyage), leaving every other
    /// index setting untouched. Use this on an existing production index,
    /// where `settings` would push the whole definition and reindex.
    Embedder {
        /// openai | voyage
        kind: String,
    },
    /// Fetch the pages stories link to and store extracted article text on
    /// the documents (embedding fodder — not full-text indexed)
    Enrich {
        /// Max characters of extracted content to keep per document
        #[arg(long, env = "ENRICH_MAX_CHARS", default_value_t = 4000)]
        max_chars: usize,
        /// Stop after attempting this many documents (useful for testing)
        #[arg(long)]
        limit: Option<u64>,
        /// Page extractor. Every page is fetched and extracted locally first
        /// (free); `auto` adds a Cloudflare browser-render fallback for pages
        /// that yield no text when CLOUDFLARE_ACCOUNT_ID + CLOUDFLARE_API_TOKEN
        /// are set, `cloudflare` requires them, `local` never uses them
        #[arg(long, env = "ENRICH_EXTRACTOR", default_value = "auto")]
        extractor: String,
        /// Only enrich stories posted on or after this date (YYYY-MM-DD)
        #[arg(long, env = "ENRICH_SINCE")]
        since: Option<String>,
        /// Only enrich stories posted in the last N days
        #[arg(long, conflicts_with = "since")]
        since_days: Option<u64>,
        /// Keep running once the backlog drains, picking up newly indexed
        /// stories every --interval seconds instead of exiting
        #[arg(long, env = "ENRICH_WATCH")]
        watch: bool,
        /// Poll interval for --watch, in seconds
        #[arg(long, env = "ENRICH_INTERVAL", default_value_t = 60)]
        interval: u64,
        /// Concurrent Cloudflare renders. Raise it to your plan's limit; 429s
        /// are retried with backoff, so overshooting slows down, not fails.
        #[arg(long, env = "ENRICH_CF_CONCURRENCY", default_value_t = 6)]
        cf_concurrency: usize,
    },
    /// Index items from --from (default: current maxitem) down to --to (default: 1)
    Backfill {
        /// Highest item id to index (default: current maxitem)
        #[arg(long)]
        from: Option<u64>,
        /// Lowest item id to index
        #[arg(long, default_value_t = 1)]
        to: u64,
        /// Shortcut: only index the most recent N items
        #[arg(long, conflicts_with_all = ["from", "to"])]
        recent: Option<u64>,
        /// Shortcut: index everything posted in the last N days
        #[arg(long, conflicts_with_all = ["from", "to", "recent"])]
        since_days: Option<u64>,
    },
    /// Follow new and updated items forever
    Sync {
        /// Poll interval in seconds
        #[arg(long, env = "SYNC_INTERVAL", default_value_t = 30)]
        interval: u64,
    },
    /// Apply settings, then run backfill and live sync concurrently.
    /// Intended as the long-running service entrypoint.
    Run {
        /// Limit the backfill to the most recent N items (unset = full corpus)
        #[arg(long, env = "BACKFILL_RECENT")]
        recent: Option<u64>,
        /// Poll interval for live sync, in seconds
        #[arg(long, env = "SYNC_INTERVAL", default_value_t = 30)]
        interval: u64,
        /// Also enrich story content continuously, alongside backfill and
        /// sync, so newly indexed stories get article text without a
        /// separate `enrich` invocation.
        #[arg(long, env = "ENRICH_ON_SYNC")]
        enrich: bool,
        /// Floor for --enrich, as a date (YYYY-MM-DD). Older stories are
        /// left alone — crawling the full corpus is a multi-day job.
        #[arg(long, env = "ENRICH_SINCE")]
        enrich_since: Option<String>,
        /// Max characters of extracted content to keep, for --enrich
        #[arg(long, env = "ENRICH_MAX_CHARS", default_value_t = 4000)]
        enrich_max_chars: usize,
        /// Concurrent Cloudflare renders, for --enrich
        #[arg(long, env = "ENRICH_CF_CONCURRENCY", default_value_t = 6)]
        enrich_cf_concurrency: usize,
    },
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Next (highest) id the backfill still has to process.
    backfill_cursor: Option<u64>,
    /// Lowest id the backfill will go down to.
    backfill_floor: Option<u64>,
    /// Highest id already covered by the sync loop.
    sync_last_max: Option<u64>,
}

struct Ctx {
    hn: reqwest::Client,
    meili: Meili,
    concurrency: usize,
    batch_size: usize,
    state_file: PathBuf,
    state: Mutex<State>,
}

impl Ctx {
    async fn save_state(&self) -> Result<()> {
        let state = self.state.lock().await;
        let json = serde_json::to_vec_pretty(&*state)?;
        let tmp = self.state_file.with_extension("json.tmp");
        tokio::fs::write(&tmp, &json).await?;
        tokio::fs::rename(&tmp, &self.state_file).await?;
        Ok(())
    }

    /// Fetch a set of ids concurrently and index whatever converts to a doc.
    /// Returns the number of documents indexed.
    async fn index_ids(&self, ids: Vec<u64>) -> Result<usize> {
        let docs: Vec<_> = stream::iter(ids)
            .map(|id| {
                let client = self.hn.clone();
                async move {
                    match hn::fetch_item(&client, id).await {
                        Ok(item) => item.and_then(hn::to_doc),
                        Err(e) => {
                            warn!("skipping item {id}: {e:#}");
                            None
                        }
                    }
                }
            })
            .buffer_unordered(self.concurrency)
            .filter_map(|doc| async move { doc })
            .collect()
            .await;
        self.meili.add_documents(&docs).await?;
        Ok(docs.len())
    }
}

async fn backfill(ctx: &Ctx, from: u64, to: u64) -> Result<()> {
    let total = from.saturating_sub(to) + 1;
    info!("backfill: items {to}..={from} ({total} ids)");
    let started = Instant::now();
    let mut processed: u64 = 0;
    let mut cursor = from;

    loop {
        let low = cursor.saturating_sub(ctx.batch_size as u64 - 1).max(to);
        let ids: Vec<u64> = (low..=cursor).rev().collect();
        let chunk_len = ids.len() as u64;
        let docs = ctx.index_ids(ids).await?;
        processed += chunk_len;

        {
            let mut state = ctx.state.lock().await;
            state.backfill_cursor = low.checked_sub(1).filter(|c| *c >= to);
            state.backfill_floor = Some(to);
        }
        ctx.save_state().await?;

        let rate = processed as f64 / started.elapsed().as_secs_f64().max(0.001);
        let remaining = low.saturating_sub(to) as f64 / rate.max(1.0);
        info!(
            "backfill: {processed}/{total} ids ({docs} docs in last chunk, {rate:.0} ids/s, ~{} left)",
            human_duration(remaining)
        );

        if low <= to {
            info!(
                "backfill complete: {processed} ids in {:?}",
                started.elapsed()
            );
            return Ok(());
        }
        cursor = low - 1;
    }
}

async fn sync(ctx: &Ctx, interval: u64) -> Result<()> {
    let mut last_max = {
        let state = ctx.state.lock().await;
        state.sync_last_max
    };
    if last_max.is_none() {
        last_max = Some(hn::max_item(&ctx.hn).await?);
        info!("sync: starting from maxitem {}", last_max.unwrap());
    }

    loop {
        let tick = async {
            let max = hn::max_item(&ctx.hn).await?;
            let prev = last_max.unwrap_or(max);
            let mut ids: Vec<u64> = if max > prev {
                (prev + 1..=max).collect()
            } else {
                vec![]
            };
            let new_count = ids.len();
            ids.extend(hn::updated_items(&ctx.hn).await.unwrap_or_default());
            ids.sort_unstable();
            ids.dedup();

            let docs = ctx.index_ids(ids).await?;
            last_max = Some(max);
            {
                let mut state = ctx.state.lock().await;
                state.sync_last_max = last_max;
            }
            ctx.save_state().await?;
            info!("sync: {new_count} new ids, {docs} docs indexed (maxitem {max})");
            anyhow::Ok(())
        };
        if let Err(e) = tick.await {
            warn!("sync tick failed, will retry: {e:#}");
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
}

/// Resolve the backfill range, preferring a saved checkpoint when its floor
/// still matches what was asked for.
async fn resolve_backfill_range(
    ctx: &Ctx,
    from: Option<u64>,
    to: u64,
) -> Result<Option<(u64, u64)>> {
    let state = ctx.state.lock().await;
    if from.is_none() {
        if let (Some(cursor), Some(floor)) = (state.backfill_cursor, state.backfill_floor) {
            if floor == to {
                info!("resuming backfill from checkpoint at id {cursor}");
                return Ok(Some((cursor, to)));
            }
        }
        if state.backfill_floor == Some(to) && state.backfill_cursor.is_none() {
            info!("backfill already complete for floor {to}, nothing to do");
            return Ok(None);
        }
    }
    drop(state);
    let from = match from {
        Some(f) => f,
        None => hn::max_item(&ctx.hn).await?,
    };
    Ok(Some((from, to)))
}

/// Per-batch tally of how pages were obtained, so each batch's log line says
/// why it was slow or failing — throttling, timeouts, or plain slow renders —
/// rather than only how many pages came back.
#[derive(Default)]
struct BatchStats {
    cf_attempts: u64,
    cf_content: u64,
    cf_empty: u64,
    cf_throttled: u64,
    cf_http: u64,
    /// Which non-429 statuses came back, and how often.
    cf_http_codes: std::collections::BTreeMap<u16, u64>,
    cf_timeout: u64,
    cf_transport: u64,
    throttled_retries: u64,
    cf_secs: f64,
    local_attempts: u64,
    local_content: u64,
}

impl BatchStats {
    fn record_cloudflare(&mut self, attempt: &enrich::CfAttempt) {
        self.cf_attempts += 1;
        self.throttled_retries += u64::from(attempt.throttled_retries);
        self.cf_secs += attempt.elapsed.as_secs_f64();
        match &attempt.outcome {
            enrich::CfOutcome::Content(_) => self.cf_content += 1,
            enrich::CfOutcome::Empty => self.cf_empty += 1,
            enrich::CfOutcome::Throttled => self.cf_throttled += 1,
            enrich::CfOutcome::Http(code) => {
                self.cf_http += 1;
                *self.cf_http_codes.entry(*code).or_default() += 1;
            }
            enrich::CfOutcome::Timeout => self.cf_timeout += 1,
            enrich::CfOutcome::Transport => self.cf_transport += 1,
        }
    }

    fn merge(&mut self, other: &BatchStats) {
        self.cf_attempts += other.cf_attempts;
        self.cf_content += other.cf_content;
        self.cf_empty += other.cf_empty;
        self.cf_throttled += other.cf_throttled;
        self.cf_http += other.cf_http;
        for (code, n) in &other.cf_http_codes {
            *self.cf_http_codes.entry(*code).or_default() += n;
        }
        self.cf_timeout += other.cf_timeout;
        self.cf_transport += other.cf_transport;
        self.throttled_retries += other.throttled_retries;
        self.cf_secs += other.cf_secs;
        self.local_attempts += other.local_attempts;
        self.local_content += other.local_content;
    }

    fn summary(&self, cloudflare: bool) -> String {
        let local = format!(", local {}/{} ok", self.local_content, self.local_attempts);
        if !cloudflare || self.cf_attempts == 0 {
            return local;
        }
        let avg = self.cf_secs / self.cf_attempts as f64;
        let codes = if self.cf_http_codes.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = self
                .cf_http_codes
                .iter()
                .map(|(code, n)| format!("{code}×{n}"))
                .collect();
            format!(" ({})", list.join(", "))
        };
        format!(
            "{local}, cloudflare fallback {} ok / {} empty / {} throttled-out / \
             {} http-err{codes} / {} timeout / {} transport, {} retries on 429, \
             avg {avg:.1}s per render",
            self.cf_content,
            self.cf_empty,
            self.cf_throttled,
            self.cf_http,
            self.cf_timeout,
            self.cf_transport,
            self.throttled_retries,
        )
    }
}

/// Repeatedly pull story documents needing enrichment, fetch the pages they
/// link to, extract the main article text, and write it back as a partial
/// document update ({id, content, enrich_gen}). Failed fetches are still
/// stamped with the generation so they aren't retried forever.
///
/// `since` limits the work to stories created at or after that unix second;
/// `watch` keeps the loop alive once the backlog drains, re-polling at that
/// interval so stories indexed later get enriched too.
async fn enrich_loop(
    ctx: &Ctx,
    max_chars: usize,
    limit: Option<u64>,
    cloudflare: Option<enrich::Cloudflare>,
    since: Option<i64>,
    watch: Option<Duration>,
    cf_concurrency: usize,
) -> Result<()> {
    // Plain fetches must fail fast — a slow server shouldn't hold a slot the
    // next page could use — while a browser render legitimately takes tens
    // of seconds. Hence two clients with different timeouts.
    let pages = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("HackerSearchBot/0.1 (article-content enrichment)")
        .build()?;
    let renders = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    // Every page gets the free local fetch first; a plain GET handles the
    // large majority of HN links. Only pages it can't extract (JS shells, bot
    // walls) go to Cloudflare, which bills per render. Its plan-bound
    // concurrency is enforced with a semaphore, so it caps the renders
    // without throttling the local fetches down to the same number.
    let fetch_concurrency = ctx.concurrency.min(32);
    let cf_concurrency = cf_concurrency.max(1);
    let cf_permits = Arc::new(tokio::sync::Semaphore::new(cf_concurrency));
    // Nothing is written back until a whole batch finishes, so the batch is
    // the unit of both progress logging and loss on restart; this keeps both
    // to roughly a minute.
    let max_batch: u64 = 250;
    info!(
        "enrich: extractor = {} (fetch concurrency {fetch_concurrency}, batches of {max_batch})",
        if cloudflare.is_some() {
            format!("local first, cloudflare fallback ({cf_concurrency} concurrent renders)")
        } else {
            "local only".to_string()
        }
    );

    let started = Instant::now();
    let mut attempted: u64 = 0;
    let mut extracted: u64 = 0;
    let mut described: u64 = 0;

    loop {
        let batch_size = match limit {
            Some(cap) => (cap - attempted).min(max_batch) as usize,
            None => max_batch as usize,
        };
        if batch_size == 0 {
            break;
        }
        let batch = ctx.meili.fetch_enrichable(batch_size, since).await?;
        if batch.is_empty() {
            // Nothing eligible right now. In watch mode that just means the
            // backlog is drained — wait for sync to index more and re-poll.
            match watch {
                Some(interval) => {
                    tokio::time::sleep(interval).await;
                    continue;
                }
                None => break,
            }
        }
        let batch_len = batch.len() as u64;

        let crawl_started = Instant::now();
        let results: Vec<(serde_json::Value, BatchStats)> = stream::iter(batch)
            .map(|story| {
                let pages = pages.clone();
                let renders = renders.clone();
                let cloudflare = cloudflare.clone();
                let cf_permits = cf_permits.clone();
                async move {
                    let mut stats = BatchStats::default();
                    let url = &story.url;
                    // 1. Plain fetch + local extraction. Free, and it yields
                    //    the page's own description even when the body is a
                    //    JavaScript shell with no text to extract.
                    let page = match enrich::fetch_page(&pages, url).await {
                        Ok(Some(html)) => Some(enrich::extract_page(
                            &html,
                            max_chars,
                            story.title.as_deref(),
                        )),
                        Ok(None) => None,
                        Err(e) => {
                            tracing::debug!("enrich: fetch failed for {url}: {e:#}");
                            None
                        }
                    };
                    let (mut content, description) =
                        page.map_or((None, None), |p| (p.content, p.description));
                    stats.local_attempts += 1;
                    stats.local_content += u64::from(content.is_some());

                    // 2. Only what the plain fetch couldn't extract is sent to
                    //    the billed browser.
                    if content.is_none() {
                        if let Some(cf) = &cloudflare {
                            let _permit = cf_permits
                                .acquire()
                                .await
                                .expect("semaphore is never closed");
                            let attempt =
                                enrich::markdown_via_cloudflare(&renders, cf, url, max_chars).await;
                            stats.record_cloudflare(&attempt);
                            if let enrich::CfOutcome::Content(text) = attempt.outcome {
                                content = Some(text);
                            }
                        }
                    }

                    // `content` and `description` are deliberately tri-state:
                    //   absent      — never attempted
                    //   ""          — attempted, nothing extractable
                    //   non-empty   — extracted text
                    // Writing "" (rather than leaving the field off) is what
                    // makes a permanent extraction failure distinguishable
                    // from a story the crawler has not reached yet.
                    let update = serde_json::json!({
                        "id": story.id,
                        "enrich_gen": meili::ENRICH_GENERATION,
                        "content": content.unwrap_or_default(),
                        "description": description.unwrap_or_default(),
                    });
                    (update, stats)
                }
            })
            .buffer_unordered(fetch_concurrency)
            .collect()
            .await;
        let crawl_secs = crawl_started.elapsed().as_secs_f64();
        let mut stats = BatchStats::default();
        let updates: Vec<serde_json::Value> = results
            .into_iter()
            .map(|(update, doc_stats)| {
                stats.merge(&doc_stats);
                update
            })
            .collect();

        let non_empty = |field: &str| {
            updates
                .iter()
                .filter(|u| u[field].as_str().is_some_and(|v| !v.is_empty()))
                .count() as u64
        };
        extracted += non_empty("content");
        described += non_empty("description");
        attempted += batch_len;

        // The next fetch_enrichable relies on the `enrich_gen` stamps being
        // visible, so wait for the update task to finish. On a busy index this
        // wait can dominate the batch — it sits behind every queued task —
        // which is why it is timed separately from the crawl.
        let write_started = Instant::now();
        if let Some(task) = ctx.meili.add_documents(&updates).await? {
            ctx.meili.wait_for_task(task).await?;
        }
        let write_secs = write_started.elapsed().as_secs_f64();

        let rate = attempted as f64 / started.elapsed().as_secs_f64().max(0.001);
        info!(
            "enrich: {attempted} attempted, {extracted} with content, {described} with \
             description ({rate:.2} docs/s) \
             | batch: crawl {crawl_secs:.0}s, write+wait {write_secs:.0}s{}",
            stats.summary(cloudflare.is_some())
        );
        if cloudflare.is_some()
            && stats.cf_attempts > 0
            && stats.throttled_retries * 4 > stats.cf_attempts
        {
            warn!(
                "enrich: Cloudflare is throttling ({} 429s over {} renders) — \
                 lower ENRICH_CF_CONCURRENCY",
                stats.throttled_retries, stats.cf_attempts
            );
        }
    }

    info!(
        "enrich complete: {extracted}/{attempted} documents got article content, \
         {described} a description, in {:?}",
        started.elapsed()
    );
    Ok(())
}

/// Resolve the `--since` / `--since-days` pair into a unix-seconds floor.
fn resolve_since(since: Option<&str>, since_days: Option<u64>) -> Result<Option<i64>> {
    if let Some(date) = since {
        return Ok(Some(parse_date(date)?));
    }
    if let Some(days) = since_days {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs() as i64;
        return Ok(Some(now - (days as i64) * 86_400));
    }
    Ok(None)
}

/// Parse `YYYY-MM-DD` into the unix second of its UTC midnight.
fn parse_date(raw: &str) -> Result<i64> {
    let parts: Vec<&str> = raw.split('-').collect();
    if parts.len() != 3 {
        anyhow::bail!("expected a date as YYYY-MM-DD, got '{raw}'");
    }
    let year: i64 = parts[0].parse().context("parsing year")?;
    let month: i64 = parts[1].parse().context("parsing month")?;
    let day: i64 = parts[2].parse().context("parsing day")?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        anyhow::bail!("'{raw}' is not a valid date");
    }
    Ok(days_from_civil(year, month, day) * 86_400)
}

/// Days from 1970-01-01 for a proleptic-Gregorian date (Howard Hinnant's
/// `days_from_civil`) — exact, and cheaper than a date-library dependency
/// for the one date this binary needs to parse.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Binary-search the lowest item id created at or after `cutoff`. HN ids are
/// sequential and times are monotonic enough for facet-grade boundaries.
async fn id_at_timestamp(client: &reqwest::Client, cutoff: i64, max: u64) -> Result<u64> {
    let (mut lo, mut hi) = (1u64, max);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        // Deleted items come back as null; probe forward a little to find a
        // neighbour that still has a timestamp.
        let mut time = None;
        let mut probe = mid;
        while time.is_none() && probe < hi.min(mid + 50) {
            time = hn::fetch_item(client, probe).await?.and_then(|i| i.time);
            probe += 1;
        }
        match time {
            Some(t) if t < cutoff => lo = mid,
            _ => hi = mid,
        }
    }
    Ok(hi)
}

fn human_duration(secs: f64) -> String {
    let secs = secs as u64;
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hn_indexer=info".into()),
        )
        .init();

    let cli = Cli::parse();

    let state = if cli.state_file.exists() {
        let raw = tokio::fs::read(&cli.state_file)
            .await
            .with_context(|| format!("reading state file {}", cli.state_file.display()))?;
        serde_json::from_slice(&raw).unwrap_or_default()
    } else {
        State::default()
    };

    let ctx = Arc::new(Ctx {
        hn: reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(cli.concurrency)
            .build()?,
        meili: Meili::new(cli.meili_url.clone(), cli.meili_key.clone()),
        concurrency: cli.concurrency,
        batch_size: cli.batch_size,
        state_file: cli.state_file.clone(),
        state: Mutex::new(state),
    });

    ctx.meili
        .health()
        .await
        .with_context(|| format!("cannot reach Meilisearch at {}", cli.meili_url))?;

    match cli.command {
        Command::Settings { embedder } => {
            ctx.meili.apply_settings().await?;
            if let Some(kind) = embedder {
                let _ = ctx.meili.apply_embedder(&kind).await?;
                info!(
                    "embedder '{kind}' configured — Meilisearch is now (re)embedding all documents"
                );
            }
            info!("index '{}' configured", meili::INDEX_UID);
        }
        Command::Embedder { kind } => {
            let task = ctx.meili.apply_embedder(&kind).await?;
            info!(
                "embedder '{kind}' submitted as task {} — Meilisearch is now embedding \
                 stories (never comments); follow it with GET /tasks/<uid>",
                task.map_or("?".to_string(), |t| t.to_string())
            );
        }
        Command::Enrich {
            max_chars,
            limit,
            extractor,
            since,
            since_days,
            watch,
            interval,
            cf_concurrency,
        } => {
            let cloudflare = match extractor.as_str() {
                "local" => None,
                "cloudflare" => Some(enrich::Cloudflare::from_env().ok_or_else(|| {
                    anyhow::anyhow!(
                        "extractor 'cloudflare' needs CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN"
                    )
                })?),
                "auto" => enrich::Cloudflare::from_env(),
                other => anyhow::bail!("unknown extractor '{other}' (auto|local|cloudflare)"),
            };
            let since = resolve_since(since.as_deref(), since_days)?;
            if let Some(floor) = since {
                info!("enrich: limited to stories created at or after {floor} (unix)");
            }
            ctx.meili.ensure_index().await?;
            enrich_loop(
                &ctx,
                max_chars,
                limit,
                cloudflare,
                since,
                watch.then(|| Duration::from_secs(interval)),
                cf_concurrency,
            )
            .await?;
        }
        Command::Backfill {
            from,
            to,
            recent,
            since_days,
        } => {
            ctx.meili.ensure_index().await?;
            let (from, to) = if let Some(days) = since_days {
                let max = hn::max_item(&ctx.hn).await?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs() as i64;
                let cutoff = now - (days as i64) * 86_400;
                info!("locating the id boundary for the last {days} days…");
                let boundary = id_at_timestamp(&ctx.hn, cutoff, max).await?;
                info!(
                    "last {days} days ≈ ids {boundary}..={max} ({} ids)",
                    max - boundary + 1
                );
                (max, boundary)
            } else if let Some(n) = recent {
                let max = hn::max_item(&ctx.hn).await?;
                (max, max.saturating_sub(n).max(1))
            } else {
                match resolve_backfill_range(&ctx, from, to).await? {
                    Some(range) => range,
                    None => return Ok(()),
                }
            };
            backfill(&ctx, from, to).await?;
            info!(
                "index now holds {} documents",
                ctx.meili.document_count().await?
            );
        }
        Command::Sync { interval } => {
            ctx.meili.ensure_index().await?;
            sync(&ctx, interval).await?;
        }
        Command::Run {
            recent,
            interval,
            enrich: enrich_enabled,
            enrich_since,
            enrich_max_chars,
            enrich_cf_concurrency,
        } => {
            ctx.meili.ensure_index().await?;
            let max = hn::max_item(&ctx.hn).await?;
            let to = recent.map(|n| max.saturating_sub(n).max(1)).unwrap_or(1);

            let backfill_ctx = ctx.clone();
            // A transient Meilisearch/network outage must never kill the
            // backfill for good: every attempt resumes from the checkpoint,
            // so retrying forever is safe and loses no progress.
            let backfill_task = tokio::spawn(async move {
                loop {
                    match resolve_backfill_range(&backfill_ctx, None, to).await {
                        Ok(Some((from, to))) => match backfill(&backfill_ctx, from, to).await {
                            Ok(()) => break,
                            Err(e) => {
                                warn!("backfill failed, resuming from checkpoint in 60s: {e:#}")
                            }
                        },
                        Ok(None) => break,
                        Err(e) => warn!("backfill setup failed, retrying in 60s: {e:#}"),
                    }
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            });

            // Enrichment, when enabled, runs forever in watch mode: it
            // drains whatever is eligible, then re-polls so stories the sync
            // loop indexes later get article text without a second command.
            let enrich_task = if enrich_enabled {
                let since = resolve_since(enrich_since.as_deref(), None)?;
                let cloudflare = enrich::Cloudflare::from_env();
                info!(
                    "enrich-on-sync enabled (extractor = {}, floor = {})",
                    if cloudflare.is_some() {
                        "cloudflare"
                    } else {
                        "local"
                    },
                    since.map_or("none".to_string(), |s| s.to_string())
                );
                let enrich_ctx = ctx.clone();
                Some(tokio::spawn(async move {
                    loop {
                        if let Err(e) = enrich_loop(
                            &enrich_ctx,
                            enrich_max_chars,
                            None,
                            cloudflare.clone(),
                            since,
                            Some(Duration::from_secs(60)),
                            enrich_cf_concurrency,
                        )
                        .await
                        {
                            warn!("enrich failed, retrying in 60s: {e:#}");
                        }
                        tokio::time::sleep(Duration::from_secs(60)).await;
                    }
                }))
            } else {
                None
            };

            // Sync runs forever; backfill finishes in the background.
            let sync_result = sync(&ctx, interval).await;
            backfill_task.abort();
            if let Some(task) = enrich_task {
                task.abort();
            }
            sync_result?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(outcome: enrich::CfOutcome, retries: u32, secs: u64) -> enrich::CfAttempt {
        enrich::CfAttempt {
            outcome,
            throttled_retries: retries,
            elapsed: Duration::from_secs(secs),
        }
    }

    #[test]
    fn batch_stats_attribute_every_outcome() {
        let mut batch = BatchStats::default();
        for a in [
            attempt(enrich::CfOutcome::Content("x".into()), 0, 3),
            attempt(enrich::CfOutcome::Content("y".into()), 1, 5),
            attempt(enrich::CfOutcome::Throttled, 4, 32),
            attempt(enrich::CfOutcome::Http(422), 0, 1),
            attempt(enrich::CfOutcome::Http(422), 0, 1),
            attempt(enrich::CfOutcome::Timeout, 0, 60),
        ] {
            let mut doc = BatchStats::default();
            doc.record_cloudflare(&a);
            batch.merge(&doc);
        }
        assert_eq!(batch.cf_attempts, 6);
        assert_eq!(batch.cf_content, 2);
        assert_eq!(batch.cf_throttled, 1);
        assert_eq!(batch.throttled_retries, 5);
        assert_eq!(batch.cf_timeout, 1);
        let line = batch.summary(true);
        assert!(line.contains("2 ok"), "{line}");
        assert!(line.contains("http-err (422×2)"), "{line}");
        assert!(line.contains("5 retries on 429"), "{line}");
        // (3 + 5 + 32 + 1 + 1 + 60) / 6
        assert!(line.contains("avg 17.0s"), "{line}");
    }

    #[test]
    fn parses_dates_to_utc_midnight() {
        assert_eq!(parse_date("1970-01-01").unwrap(), 0);
        // The cutoff this project actually cares about.
        assert_eq!(parse_date("2025-01-01").unwrap(), 1_735_689_600);
        // Leap day: 59 whole days after 2024-01-01.
        assert_eq!(
            parse_date("2024-02-29").unwrap(),
            parse_date("2024-01-01").unwrap() + 59 * 86_400
        );
        // Monotonic across a century boundary (2100 is not a leap year).
        assert!(parse_date("2100-03-01").unwrap() > parse_date("2100-02-28").unwrap());
    }

    #[test]
    fn rejects_malformed_dates() {
        for bad in [
            "2025",
            "2025-01",
            "2025-13-01",
            "2025-01-32",
            "not-a-date",
            "",
        ] {
            assert!(parse_date(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn since_prefers_explicit_date_and_defaults_to_none() {
        assert_eq!(resolve_since(None, None).unwrap(), None);
        assert_eq!(
            resolve_since(Some("2025-01-01"), None).unwrap(),
            Some(1_735_689_600)
        );
        // --since-days is relative, so just assert it lands in the past.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let floor = resolve_since(None, Some(30)).unwrap().unwrap();
        assert!(floor < now && floor > now - 31 * 86_400);
    }
}
