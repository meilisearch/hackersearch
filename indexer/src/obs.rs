//! Prometheus metrics exporter. Best-effort by design: indexing must never
//! die because the metrics port is taken (e.g. an ad-hoc backfill run while
//! the sync service holds 9464) or the address is malformed.

use std::net::SocketAddr;

use metrics::{describe_counter, describe_gauge, describe_histogram};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use tracing::{info, warn};

/// Parse the listener address; empty/blank means "metrics disabled".
pub fn parse_addr(addr: &str) -> Option<SocketAddr> {
    let addr = addr.trim();
    if addr.is_empty() {
        return None;
    }
    match addr.parse() {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            warn!("invalid metrics address '{addr}', metrics disabled: {e}");
            None
        }
    }
}

/// Start the Prometheus /metrics HTTP listener. Failure is logged, never fatal.
pub fn init(addr: &str) {
    let Some(addr) = parse_addr(addr) else { return };
    let builder = PrometheusBuilder::new()
        .with_http_listener(addr)
        // Real buckets, not the exporter's default rolling summaries: one
        // sync tick per 30s is sparse, and summaries can't be aggregated.
        .set_buckets_for_metric(
            Matcher::Full("hn_indexer_sync_tick_duration_seconds".into()),
            &[0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0],
        )
        .expect("bucket list is non-empty");
    match builder.install() {
        Ok(()) => {
            info!("metrics exposed at http://{addr}/metrics");
            describe();
        }
        Err(e) => {
            warn!("metrics exporter failed to start on {addr}, continuing without metrics: {e:#}")
        }
    }
}

/// HELP text for everything the loops record. Labels are all bounded enums —
/// never ids or urls (cardinality lessons already learned on this box).
fn describe() {
    describe_counter!(
        "hn_indexer_sync_ticks_total",
        "Sync loop iterations, labelled result=ok|error"
    );
    describe_counter!(
        "hn_indexer_sync_new_ids_total",
        "New item ids seen above the previous maxitem"
    );
    describe_counter!(
        "hn_indexer_sync_docs_indexed_total",
        "Documents sent to Meilisearch by the sync loop"
    );
    describe_gauge!("hn_indexer_hn_maxitem", "HN's current maxitem");
    describe_gauge!(
        "hn_indexer_sync_last_max",
        "Highest item id covered by sync; lag = hn_maxitem - this"
    );
    describe_gauge!(
        "hn_indexer_sync_last_success_timestamp_seconds",
        "Unix time of the last successful sync tick"
    );
    describe_histogram!(
        "hn_indexer_sync_tick_duration_seconds",
        "Wall-clock duration of one sync tick"
    );
    describe_counter!(
        "hn_indexer_items_fetched_total",
        "HN item fetches, labelled result=ok|null|error (null = deleted/dead/absent)"
    );
    describe_gauge!(
        "hn_indexer_backfill_active",
        "1 while a backfill loop is running in this process"
    );
    describe_gauge!(
        "hn_indexer_backfill_cursor",
        "Next (highest) id the backfill still has to process"
    );
    describe_gauge!(
        "hn_indexer_backfill_floor",
        "Lowest id the backfill will go down to"
    );
    describe_counter!(
        "hn_indexer_backfill_ids_processed_total",
        "Item ids processed by backfill"
    );
    describe_counter!(
        "hn_indexer_backfill_docs_indexed_total",
        "Documents sent to Meilisearch by backfill"
    );
    describe_counter!(
        "hn_indexer_enrich_attempted_total",
        "Documents the enrich loop attempted to fetch article content for"
    );
    describe_counter!(
        "hn_indexer_enrich_extracted_total",
        "Documents that actually got article content"
    );
    describe_counter!(
        "hn_indexer_meili_writes_total",
        "Meilisearch document-write calls, labelled result=ok|error"
    );
}

#[cfg(test)]
mod tests {
    use super::parse_addr;

    #[test]
    fn empty_or_blank_disables() {
        assert!(parse_addr("").is_none());
        assert!(parse_addr("   ").is_none());
    }

    #[test]
    fn parses_socket_addr() {
        let addr = parse_addr("127.0.0.1:9464").unwrap();
        assert_eq!(addr.port(), 9464);
        assert!(addr.ip().is_loopback());
    }

    #[test]
    fn garbage_disables_instead_of_crashing() {
        assert!(parse_addr("not-an-addr").is_none());
    }
}
