use anyhow::{Context, Result};
use serde_json::json;

pub const INDEX_UID: &str = "hn";

/// Bumped whenever the extraction pipeline changes in a way that makes
/// already-stored `content` worth replacing. Documents carry the generation
/// they were enriched under in `enrich_gen`; `enrich` re-processes anything
/// stamped with an older one (or nothing at all — which covers both
/// never-enriched documents and those written before this field existed,
/// when the marker was a bare `enriched: true`).
pub const ENRICH_GENERATION: u32 = 2;

/// What gets embedded for each document: the title, plus the crawled article
/// text when there is some. Comments are never embedded.
///
/// How comments are excluded is load-bearing and non-obvious, so read this
/// before touching the template. Measured on Meilisearch v1.49:
///
/// - A fragment is SKIPPED for a document only when it outputs (`{{ … }}`) a
///   field that document does not have. HN comments have no `title` (hn.rs
///   only ever sets it from the API's `title`), so `{{ doc.title }}` is what
///   keeps every comment out of the vector store.
/// - A fragment that merely renders to an EMPTY string is NOT skipped — the
///   document is embedded as "". So wrapping this in
///   `{% if doc.type != "comment" %}…{% endif %}` looks like a stricter guard
///   but does the opposite: every comment would be embedded as the same empty
///   vector, tens of millions of identical points that surface together in
///   semantic search. Same reason `documentTemplate` can't be used at all.
/// - A missing field inside a false `{% if %}` branch does not trigger the
///   skip, which is why never-crawled stories (no `content`) still embed
///   their title.
///
/// `content` is tri-state (absent / "" / text) and Liquid treats "" as
/// truthy, hence the explicit `!= ""`.
pub const ARTICLE_FRAGMENT: &str = "{{ doc.title }}\
{% if doc.content and doc.content != \"\" %}\n{{ doc.content | truncatewords: 400 }}{% endif %}";

pub struct Meili {
    client: reqwest::Client,
    base: String,
    key: Option<String>,
}

impl Meili {
    pub fn new(base: String, key: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_string(),
            key,
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .request(method, format!("{}{}", self.base, path));
        if let Some(key) = &self.key {
            req = req.bearer_auth(key);
        }
        req
    }

    pub async fn health(&self) -> Result<()> {
        self.request(reqwest::Method::GET, "/health")
            .send()
            .await?
            .error_for_status()
            .context("Meilisearch health check failed")?;
        Ok(())
    }

    /// Create and configure the index only when it doesn't exist yet.
    /// Existing indexes are left completely untouched — no settings task,
    /// no reindex, no startup stall. Settings changes are pushed explicitly
    /// with the `settings` command.
    pub async fn ensure_index(&self) -> Result<()> {
        let resp = self
            .request(reqwest::Method::GET, &format!("/indexes/{INDEX_UID}"))
            .send()
            .await?;
        if resp.status().is_success() {
            return Ok(());
        }
        if resp.status() != reqwest::StatusCode::NOT_FOUND {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("checking index '{INDEX_UID}': {status}: {body}");
        }
        tracing::info!("index '{INDEX_UID}' does not exist — creating and configuring it");
        self.apply_settings().await
    }

    /// Create the index (idempotent) and apply the search configuration.
    pub async fn apply_settings(&self) -> Result<()> {
        // Index creation is a task; if the index already exists the task
        // fails asynchronously, which is fine — settings below still apply.
        self.request(reqwest::Method::POST, "/indexes")
            .json(&json!({ "uid": INDEX_UID, "primaryKey": "id" }))
            .send()
            .await?;

        // url, domain, and author are excluded from full-text search: url
        // is opaque link noise, domain matching produces junk relevance
        // (e.g. "medium" matching every medium.com post regardless of
        // content), and same for author names (e.g. "dan" surfacing every
        // post by user "dang"). Domain/author stay reachable via filters
        // and the dedicated facet-search endpoint instead.
        //
        // filterableAttributes lists each attribute's actual needs instead
        // of turning every feature on everywhere:
        // - facetSearch is only needed where the UI calls the facet-search
        //   endpoint (domain, author — see web/src/lib/meili.ts).
        // - comparison (<, >, >=, <=) is only needed for the numeric range
        //   filters the UI actually issues (points, created_at); everything
        //   else only ever uses equality (=, !=, EXISTS).
        let settings = json!({
            "searchableAttributes": ["title", "text"],
            "filterableAttributes": [
                { "attributePatterns": ["domain", "author"],
                  "features": { "facetSearch": true, "filter": { "equality": true, "comparison": false } } },
                { "attributePatterns": ["type", "tags", "url", "enriched", "parent"],
                  "features": { "facetSearch": false, "filter": { "equality": true, "comparison": false } } },
                { "attributePatterns": ["enrich_gen"],
                  "features": { "facetSearch": false, "filter": { "equality": true, "comparison": true } } },
                { "attributePatterns": ["points", "num_comments", "created_at"],
                  "features": { "facetSearch": false, "filter": { "equality": false, "comparison": true } } }
            ],
            "sortableAttributes": ["created_at", "points", "num_comments"],
            // sort sits BEFORE attribute (default is after): when a query
            // asks for an explicit sort — the UI's Newest/Points modes and
            // the ghost-completion query's points:desc — it should dominate
            // over which attribute/position the terms matched in. Queries
            // without a sort param are unaffected.
            "rankingRules": [
                "words", "typo", "proximity", "sort", "attribute", "exactness",
                "points:desc"
            ],
            // Terms only need to share an attribute, not sit at an exact
            // word distance — cheaper to compute and title/text/domain are
            // independent fields anyway, so exact cross-field distance was
            // never meaningful.
            "proximityPrecision": "byAttribute",
            "faceting": { "maxValuesPerFacet": 100 },
            "pagination": { "maxTotalHits": 10000 },
            "typoTolerance": { "minWordSizeForTypos": { "oneTypo": 4, "twoTypos": 9 } }
        });
        let task: serde_json::Value = self
            .request(
                reqwest::Method::PATCH,
                &format!("/indexes/{INDEX_UID}/settings"),
            )
            .json(&settings)
            .send()
            .await?
            .error_for_status()
            .context("applying index settings")?
            .json()
            .await?;
        // Settings are applied asynchronously; later calls (filters on the
        // newly filterable attributes) need them to actually be live.
        if let Some(uid) = task["taskUid"].as_u64() {
            self.wait_for_task(uid).await?;
        }
        Ok(())
    }

    /// Upsert documents. Uses PUT (add-or-UPDATE) rather than POST
    /// (add-or-replace) so re-syncing an item never wipes fields the item
    /// payload doesn't carry — notably the enrichment `content` field.
    /// Returns the task uid.
    pub async fn add_documents<T: serde::Serialize>(&self, docs: &[T]) -> Result<Option<u64>> {
        if docs.is_empty() {
            return Ok(None);
        }
        // ~5 minutes of patience: remote instances can throttle or briefly
        // stall under sustained ingestion, and giving up here aborts the
        // caller's whole pipeline.
        let mut delay = std::time::Duration::from_millis(500);
        let mut last_err: Option<anyhow::Error> = None;
        for _ in 0..10 {
            let resp = self
                .request(
                    reqwest::Method::PUT,
                    &format!("/indexes/{INDEX_UID}/documents?primaryKey=id"),
                )
                .json(docs)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    let task: serde_json::Value = r.json().await?;
                    return Ok(task["taskUid"].as_u64());
                }
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    last_err = Some(anyhow::anyhow!("meilisearch {status}: {body}"));
                }
                Err(e) => last_err = Some(e.into()),
            }
            tokio::time::sleep(delay).await;
            delay = delay
                .saturating_mul(2)
                .min(std::time::Duration::from_secs(60));
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("add_documents: retries exhausted")))
    }

    /// Block until a task reaches a terminal state.
    pub async fn wait_for_task(&self, uid: u64) -> Result<()> {
        loop {
            let task: serde_json::Value = self
                .request(reqwest::Method::GET, &format!("/tasks/{uid}"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            match task["status"].as_str().unwrap_or("") {
                "succeeded" => return Ok(()),
                "failed" | "canceled" => {
                    anyhow::bail!("task {uid} ended as {}: {}", task["status"], task["error"])
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
            }
        }
    }

    /// Fetch (id, url) pairs of documents that still need enrichment:
    /// link stories stamped with an older `enrich_gen` than the current one,
    /// optionally limited to those created at or after `since` (unix secs).
    ///
    /// Only `type = "story"`: comments carry no URL, and job posts do link
    /// out — but to careers pages, which are not articles and would only add
    /// noise to the embeddings. Show HN links are stories, so they're kept;
    /// Ask HN posts are stories without a URL, so `url EXISTS` drops them.
    ///
    /// Documents drop out of this filter as they are stamped, so the caller
    /// can keep pulling batches until it comes back empty — no pagination
    /// cursor, and re-runs resume wherever the last one stopped.
    pub async fn fetch_enrichable(
        &self,
        limit: usize,
        since: Option<i64>,
    ) -> Result<Vec<(u64, String)>> {
        let mut filter = format!(
            "type = \"story\" AND url EXISTS \
             AND (enrich_gen NOT EXISTS OR enrich_gen < {ENRICH_GENERATION})"
        );
        if let Some(since) = since {
            filter.push_str(&format!(" AND created_at >= {since}"));
        }
        let body = json!({
            "filter": filter,
            "fields": ["id", "url"],
            "limit": limit,
        });
        let resp: serde_json::Value = self
            .request(
                reqwest::Method::POST,
                &format!("/indexes/{INDEX_UID}/documents/fetch"),
            )
            .json(&body)
            .send()
            .await?
            .error_for_status()
            .context("fetching enrichable documents")?
            .json()
            .await?;
        let results = resp["results"].as_array().cloned().unwrap_or_default();
        Ok(results
            .into_iter()
            .filter_map(|doc| Some((doc["id"].as_u64()?, doc["url"].as_str()?.to_string())))
            .collect())
    }

    /// Configure the `default` embedder used for semantic/hybrid search.
    ///
    /// Touches only the `embedders` setting (plus the `multimodal`
    /// experimental feature that fragments need), never the rest of the
    /// index settings — so it is safe on an index where `apply_settings()`
    /// would trigger a full reindex.
    ///
    /// Comments are never embedded; see [`ARTICLE_FRAGMENT`] for how that is
    /// enforced and why it has to be done the way it is.
    ///
    /// Returns the uid of the settings task, which is where embedding
    /// progress and failures show up.
    pub async fn apply_embedder(&self, kind: &str) -> Result<Option<u64>> {
        let (default_url, model_var, default_model, key_var) = match kind {
            "openai" => (
                "https://api.openai.com/v1/embeddings",
                "OPENAI_EMBED_MODEL",
                "text-embedding-3-small",
                "OPENAI_API_KEY",
            ),
            "voyage" => (
                "https://api.voyageai.com/v1/embeddings",
                "VOYAGE_EMBED_MODEL",
                "voyage-3.5-lite",
                "VOYAGE_API_KEY",
            ),
            // Meilisearch only supports indexing fragments on the `rest`
            // source; a huggingFace embedder can only use documentTemplate,
            // which embeds every document — comments included.
            "huggingface" => anyhow::bail!(
                "the local huggingFace embedder cannot skip comments \
                 (indexing fragments are rest-only) — use openai or voyage"
            ),
            other => anyhow::bail!("unknown embedder '{other}' (openai|voyage)"),
        };
        let non_empty = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
        let api_key =
            non_empty(key_var).with_context(|| format!("embedder '{kind}' needs {key_var}"))?;
        let model = non_empty(model_var).unwrap_or_else(|| default_model.to_string());
        // Any OpenAI-compatible endpoint works (e.g. a self-hosted gateway).
        let url = non_empty("EMBEDDER_URL").unwrap_or_else(|| default_url.to_string());

        // Meilisearch cannot infer `dimensions` for an embedder that uses
        // indexing fragments, so measure it with one real call. That call
        // doubles as a credential check: a bad key or model fails here, on
        // the operator's terminal, instead of as a failed settings task
        // after Meilisearch has already swapped in a broken embedder.
        let dimensions = match non_empty("EMBEDDER_DIMENSIONS") {
            Some(d) => d
                .parse::<usize>()
                .context("EMBEDDER_DIMENSIONS must be an integer")?,
            None => self.probe_dimensions(&url, &api_key, &model).await?,
        };
        tracing::info!("embedder: {url} model={model} dimensions={dimensions}");

        // Indexing/search fragments are gated behind this experimental flag.
        self.request(reqwest::Method::PATCH, "/experimental-features")
            .json(&json!({ "multimodal": true }))
            .send()
            .await?
            .error_for_status()
            .context("enabling the multimodal experimental feature")?;

        let embedder = json!({
            "source": "rest",
            "url": url,
            "apiKey": api_key,
            "dimensions": dimensions,
            "request": { "model": model, "input": "{{fragment}}" },
            "response": { "data": [{ "embedding": "{{embedding}}" }] },
            "indexingFragments": { "article": { "value": ARTICLE_FRAGMENT } },
            "searchFragments": { "query": { "value": "{{ q }}" } },
        });
        let task = self
            .request(
                reqwest::Method::PATCH,
                &format!("/indexes/{INDEX_UID}/settings"),
            )
            .json(&json!({ "embedders": { "default": embedder } }))
            .send()
            .await?
            .error_for_status()
            .context("applying embedder settings")?
            .json::<serde_json::Value>()
            .await?;
        // Deliberately not waited on: this one task also embeds every story
        // already in the index, which on the full corpus runs for hours.
        let uid = task["taskUid"].as_u64();
        Ok(uid)
    }

    /// Embed a throwaway string once and return the vector's length.
    async fn probe_dimensions(&self, url: &str, api_key: &str, model: &str) -> Result<usize> {
        let resp = self
            .client
            .post(url)
            .bearer_auth(api_key)
            .json(&json!({ "model": model, "input": "dimension probe" }))
            .send()
            .await
            .with_context(|| format!("reaching embedding endpoint {url}"))?;
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("embedding endpoint returned {status}: {body}");
        }
        body["data"][0]["embedding"]
            .as_array()
            .map(|v| v.len())
            .filter(|&n| n > 0)
            .with_context(|| format!("no embedding in probe response: {body}"))
    }

    pub async fn document_count(&self) -> Result<u64> {
        let stats: serde_json::Value = self
            .request(reqwest::Method::GET, &format!("/indexes/{INDEX_UID}/stats"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(stats["numberOfDocuments"].as_u64().unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Comment exclusion depends on the fragment OUTPUTTING `doc.title`
    /// unconditionally — see ARTICLE_FRAGMENT. A type-based `{% if %}` guard
    /// would make Meilisearch embed every comment as "" instead of skipping it.
    #[test]
    fn article_fragment_gates_comments_on_title() {
        assert!(ARTICLE_FRAGMENT.starts_with("{{ doc.title }}"));
        assert!(
            !ARTICLE_FRAGMENT.contains("doc.type"),
            "a type-based guard embeds comments as empty strings rather than skipping them"
        );
        assert!(
            !ARTICLE_FRAGMENT.contains("doc.text"),
            "comment text must never be embedded"
        );
    }
}
