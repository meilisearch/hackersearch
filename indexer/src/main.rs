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
        /// Also configure an embedder for semantic search: huggingface | openai
        #[arg(long)]
        embedder: Option<String>,
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
        /// Page extractor: auto (cloudflare when CLOUDFLARE_ACCOUNT_ID +
        /// CLOUDFLARE_API_TOKEN are set, else local), local, or cloudflare
        #[arg(long, env = "ENRICH_EXTRACTOR", default_value = "auto")]
        extractor: String,
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

/// Repeatedly pull un-enriched story documents from the index, fetch the
/// pages they link to, extract the main article text, and write it back as
/// a partial document update ({id, content, enriched}). Failed fetches are
/// still marked `enriched` so they aren't retried forever.
async fn enrich_loop(
    ctx: &Ctx,
    max_chars: usize,
    limit: Option<u64>,
    cloudflare: Option<enrich::Cloudflare>,
) -> Result<()> {
    let pages = reqwest::Client::builder()
        .timeout(Duration::from_secs(if cloudflare.is_some() {
            60
        } else {
            15
        }))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("HackerSearchBot/0.1 (article-content enrichment)")
        .build()?;
    // Browser rendering is a scarcer resource than plain GETs.
    let fetch_concurrency = if cloudflare.is_some() {
        ctx.concurrency.min(6)
    } else {
        ctx.concurrency.min(32)
    };
    info!(
        "enrich: extractor = {}",
        if cloudflare.is_some() {
            "cloudflare browser rendering (local fallback)"
        } else {
            "local"
        }
    );

    let started = Instant::now();
    let mut attempted: u64 = 0;
    let mut extracted: u64 = 0;

    loop {
        let batch_size = match limit {
            Some(cap) => (cap - attempted).min(500) as usize,
            None => 500,
        };
        if batch_size == 0 {
            break;
        }
        let batch = ctx.meili.fetch_enrichable(batch_size).await?;
        if batch.is_empty() {
            break;
        }
        let batch_len = batch.len() as u64;

        let updates: Vec<serde_json::Value> = stream::iter(batch)
            .map(|(id, url)| {
                let pages = pages.clone();
                let cloudflare = cloudflare.clone();
                async move {
                    // Prefer the rendered-browser markdown when available;
                    // fall back to a plain fetch + local extraction.
                    let mut content = match &cloudflare {
                        Some(cf) => enrich::markdown_via_cloudflare(&pages, cf, &url, max_chars)
                            .await
                            .unwrap_or_default(),
                        None => None,
                    };
                    if content.is_none() {
                        content = match enrich::fetch_page(&pages, &url).await {
                            Ok(Some(html)) => enrich::extract_content(&html, max_chars),
                            Ok(None) => None,
                            Err(e) => {
                                tracing::debug!("enrich: fetch failed for {url}: {e:#}");
                                None
                            }
                        };
                    }
                    let mut update = serde_json::json!({ "id": id, "enriched": true });
                    if let Some(content) = content {
                        update["content"] = content.into();
                    }
                    update
                }
            })
            .buffer_unordered(fetch_concurrency)
            .collect()
            .await;

        extracted += updates
            .iter()
            .filter(|u| u.get("content").is_some())
            .count() as u64;
        attempted += batch_len;

        // The next fetch_enrichable relies on the `enriched` flag being
        // visible, so wait for the update task to finish.
        if let Some(task) = ctx.meili.add_documents(&updates).await? {
            ctx.meili.wait_for_task(task).await?;
        }

        let rate = attempted as f64 / started.elapsed().as_secs_f64().max(0.001);
        info!("enrich: {attempted} attempted, {extracted} with content ({rate:.0} docs/s)");
    }

    info!(
        "enrich complete: {extracted}/{attempted} documents got article content in {:?}",
        started.elapsed()
    );
    Ok(())
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
                ctx.meili.apply_embedder(&kind).await?;
                info!(
                    "embedder '{kind}' configured — Meilisearch is now (re)embedding all documents"
                );
            }
            info!("index '{}' configured", meili::INDEX_UID);
        }
        Command::Enrich {
            max_chars,
            limit,
            extractor,
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
            ctx.meili.apply_settings().await?;
            enrich_loop(&ctx, max_chars, limit, cloudflare).await?;
        }
        Command::Backfill {
            from,
            to,
            recent,
            since_days,
        } => {
            ctx.meili.apply_settings().await?;
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
            ctx.meili.apply_settings().await?;
            sync(&ctx, interval).await?;
        }
        Command::Run { recent, interval } => {
            ctx.meili.apply_settings().await?;
            let max = hn::max_item(&ctx.hn).await?;
            let to = recent.map(|n| max.saturating_sub(n).max(1)).unwrap_or(1);

            let backfill_ctx = ctx.clone();
            let backfill_task = tokio::spawn(async move {
                match resolve_backfill_range(&backfill_ctx, None, to).await {
                    Ok(Some((from, to))) => {
                        if let Err(e) = backfill(&backfill_ctx, from, to).await {
                            warn!("backfill failed: {e:#}");
                        }
                    }
                    Ok(None) => {}
                    Err(e) => warn!("backfill setup failed: {e:#}"),
                }
            });

            // Sync runs forever; backfill finishes in the background.
            let sync_result = sync(&ctx, interval).await;
            backfill_task.abort();
            sync_result?;
        }
    }

    Ok(())
}
