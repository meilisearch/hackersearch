//! One-time migration from the single pre-split `hn` index to `hn-stories` +
//! `hn-comments`.
//!
//! Only the small side moves. Stories are ~10% of the corpus, so they are
//! copied into a fresh `hn-stories`; the ~40M comments never leave `hn`,
//! which is emptied of stories and then renamed to `hn-comments` in place.
//! Re-backfilling instead would take ~17 hours and throw away every crawled
//! `content` / `description` — the copy keeps them, since documents are
//! moved whole.
//!
//! Every step is safe to re-run: the copy is an upsert checkpointed in the
//! indexer state file, the delete and the rename are only issued once the
//! copy is verified, and a finished split is detected and left alone.
//! Nothing else may write to `hn` meanwhile — `refuse_legacy` stops the
//! other commands, but an OLD indexer binary still running would not be
//! stopped by it, so shut it down first.

use std::time::Instant;

use anyhow::Result;
use tracing::{info, warn};

use crate::meili::{COMMENTS_INDEX, LEGACY_INDEX, STORIES_INDEX};
use crate::{human_duration, Ctx};

/// Everything the News tab shows: stories, jobs, polls, poll options.
const STORIES_FILTER: &str = "type != \"comment\"";

/// Pages copied between two waits on Meilisearch. The checkpoint only ever
/// advances past documents whose write task has succeeded, so this bounds
/// how much a restart redoes — and how far the copy can run ahead of
/// indexing.
const PAGES_PER_CHECKPOINT: u64 = 20;

pub async fn run(ctx: &Ctx) -> Result<()> {
    let meili = &ctx.meili;
    let legacy = meili.index_exists(LEGACY_INDEX).await?;
    let stories = meili.index_exists(STORIES_INDEX).await?;
    let comments = meili.index_exists(COMMENTS_INDEX).await?;

    if !legacy {
        if stories && comments {
            info!("split: already done — '{STORIES_INDEX}' and '{COMMENTS_INDEX}' exist and '{LEGACY_INDEX}' is gone");
            return Ok(());
        }
        anyhow::bail!(
            "split: there is no '{LEGACY_INDEX}' index to split. A fresh install needs no \
             migration — `hn-indexer settings` creates both indexes"
        );
    }
    if comments {
        anyhow::bail!(
            "split: '{COMMENTS_INDEX}' already exists, so '{LEGACY_INDEX}' cannot be renamed to it. \
             It was most likely created by hand or by a half-configured deploy; delete it \
             (DELETE /indexes/{COMMENTS_INDEX}) and re-run"
        );
    }
    let embedders = meili.embedder_names(LEGACY_INDEX).await?;

    // 1. A configured, empty (or partially filled, on a re-run) stories index.
    info!("split: configuring '{STORIES_INDEX}'");
    if let Some(task) = meili.configure_index(STORIES_INDEX).await? {
        meili.wait_for_task(task).await?;
    }

    // 2. Copy every non-comment document across, whole.
    let total = copy_stories(ctx).await?;

    // 3. Verify before anything destructive happens. Earlier write tasks are
    //    not waited on individually, so a failed one would only show up here.
    let copied = meili.document_count(STORIES_INDEX).await?;
    if copied < total {
        clear_checkpoint(ctx).await?;
        anyhow::bail!(
            "split: '{STORIES_INDEX}' holds {copied} documents but '{LEGACY_INDEX}' has {total} \
             to move — a write task failed (see GET /tasks?statuses=failed). Nothing was \
             deleted; re-run to copy again from the start"
        );
    }
    info!("split: verified — '{STORIES_INDEX}' holds {copied} documents");

    // 4. Remove the copied documents from the legacy index...
    info!("split: deleting {total} non-comment documents from '{LEGACY_INDEX}' (one task; can take a while)");
    let started = Instant::now();
    if let Some(task) = meili.delete_by_filter(LEGACY_INDEX, STORIES_FILTER).await? {
        meili.wait_for_task(task).await?;
    }
    info!(
        "split: deleted in {}",
        human_duration(started.elapsed().as_secs_f64())
    );

    // 5. ...and rename what's left — only comments — in place.
    info!("split: renaming '{LEGACY_INDEX}' to '{COMMENTS_INDEX}'");
    if let Some(task) = meili.rename_index(LEGACY_INDEX, COMMENTS_INDEX).await? {
        meili.wait_for_task(task).await?;
    }
    clear_checkpoint(ctx).await?;

    // 6. Trim the settings down to what comments need. That rewrites the
    //    index, so it runs in the background: searches keep being served
    //    under the old settings until it lands.
    let task = meili.configure_index(COMMENTS_INDEX).await?;
    info!(
        "split: done. '{COMMENTS_INDEX}' is being re-configured for comments in the background \
         (task {}); searches keep working meanwhile",
        task.map_or("?".to_string(), |t| t.to_string())
    );
    if !embedders.is_empty() {
        warn!(
            "split: '{LEGACY_INDEX}' had embedder(s) {embedders:?}; vectors are not copied, so \
             run `hn-indexer embedder <openai|voyage>` to embed '{STORIES_INDEX}'"
        );
    }
    Ok(())
}

/// Page through the legacy index's non-comment documents and upsert them
/// into the stories index, resuming from the checkpoint. Returns how many
/// documents there are to move in total.
async fn copy_stories(ctx: &Ctx) -> Result<u64> {
    let meili = &ctx.meili;
    let page_size = ctx.batch_size;
    let (_, total) = meili
        .fetch_documents(LEGACY_INDEX, STORIES_FILTER, 0, 1)
        .await?;
    let mut offset = ctx.state.lock().await.split_copied.unwrap_or(0);
    if offset > 0 {
        info!("split: resuming the copy at {offset}/{total}");
    } else {
        info!("split: copying {total} non-comment documents into '{STORIES_INDEX}'");
    }

    let started = Instant::now();
    let resumed_at = offset;
    let mut pending: Option<u64> = None;
    let mut pages: u64 = 0;
    loop {
        let (docs, _) = meili
            .fetch_documents(LEGACY_INDEX, STORIES_FILTER, offset, page_size)
            .await?;
        let done = docs.is_empty();
        if !done {
            pending = meili.add_documents(STORIES_INDEX, &docs).await?.or(pending);
            offset += docs.len() as u64;
            pages += 1;
        }
        if done || pages.is_multiple_of(PAGES_PER_CHECKPOINT) {
            if let Some(task) = pending.take() {
                meili.wait_for_task(task).await?;
            }
            ctx.state.lock().await.split_copied = Some(offset);
            ctx.save_state().await?;
            let rate = (offset - resumed_at) as f64 / started.elapsed().as_secs_f64().max(0.001);
            info!(
                "split: {offset}/{total} copied ({rate:.0} docs/s, ~{} left)",
                human_duration(total.saturating_sub(offset) as f64 / rate.max(1.0))
            );
        }
        if done {
            return Ok(total);
        }
    }
}

async fn clear_checkpoint(ctx: &Ctx) -> Result<()> {
    ctx.state.lock().await.split_copied = None;
    ctx.save_state().await
}
