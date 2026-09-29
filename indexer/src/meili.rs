use anyhow::{Context, Result};
use serde_json::json;

/// Stories, Ask/Show/Launch HN, jobs, polls and poll options — everything
/// the News tab searches. Small (~10% of the corpus), and the only index with
/// enrichment and an embedder.
pub const STORIES_INDEX: &str = "hn-stories";
/// Comments only — the Comments tab and the thread view. ~90% of the corpus,
/// with deliberately lean settings: no scores, tags or domains exist here.
pub const COMMENTS_INDEX: &str = "hn-comments";
/// The single index everything lived in before the split. While it exists,
/// every command except `split` refuses to run — see [`Meili::refuse_legacy`].
pub const LEGACY_INDEX: &str = "hn";

/// Which index an item belongs in, by its HN `type`.
pub fn index_for(kind: &str) -> &'static str {
    if kind == "comment" {
        COMMENTS_INDEX
    } else {
        STORIES_INDEX
    }
}

/// Bumped whenever the extraction pipeline changes in a way that makes
/// already-stored `content` worth replacing. Documents carry the generation
/// they were enriched under in `enrich_gen`; `enrich` re-processes anything
/// stamped with an older one (or nothing at all — which covers both
/// never-enriched documents and those written before this field existed,
/// when the marker was a bare `enriched: true`).
///
/// History: 2 = Cloudflare-first crawl; 3 = local-first crawl that also
/// stores the page's own `description`.
pub const ENRICH_GENERATION: u32 = 3;

/// What gets embedded for each document: the title, plus the page's own
/// description and the crawled article text when there are some.
///
/// Comments are never embedded: they live in [`COMMENTS_INDEX`], which has no
/// embedder. Within the stories index, poll options still have no title, and
/// how they are excluded is load-bearing and non-obvious, so read this before
/// touching the template. Measured on Meilisearch v1.49:
///
/// - A fragment is SKIPPED for a document only when it outputs (`{{ … }}`) a
///   field that document does not have. Poll options have no `title` (hn.rs
///   only ever sets it from the API's `title`), so `{{ doc.title }}` is what
///   keeps them out of the vector store.
/// - A fragment that merely renders to an EMPTY string is NOT skipped — the
///   document is embedded as "". So a `{% if %}` guard around the title looks
///   stricter but does the opposite: every untitled document would be
///   embedded as the same empty vector, identical points that surface
///   together in semantic search. Same reason `documentTemplate` isn't used.
/// - A missing field inside a false `{% if %}` branch does not trigger the
///   skip, which is why never-crawled stories (no `content`) still embed
///   their title.
///
/// `content` and `description` are tri-state (absent / "" / text) and Liquid
/// treats "" as truthy, hence the explicit `!= ""`. Both sit inside `{% if %}`
/// branches, so a story missing either still embeds its title.
pub const ARTICLE_FRAGMENT: &str = "{{ doc.title }}\
{% if doc.description and doc.description != \"\" %}\n{{ doc.description }}{% endif %}\
{% if doc.content and doc.content != \"\" %}\n{{ doc.content | truncatewords: 400 }}{% endif %}";

/// A story waiting to be enriched.
pub struct Enrichable {
    pub id: u64,
    pub url: String,
    /// Used to discard page descriptions that only repeat the title.
    pub title: Option<String>,
}

/// Search configuration of [`STORIES_INDEX`].
fn stories_settings() -> serde_json::Value {
    // url, domain, and author are excluded from full-text search: url is
    // opaque link noise, domain matching produces junk relevance (e.g.
    // "medium" matching every medium.com post regardless of content), and
    // same for author names (e.g. "dan" surfacing every post by user
    // "dang"). Domain/author stay reachable via filters and the dedicated
    // facet-search endpoint instead.
    //
    // filterableAttributes lists each attribute's actual needs instead of
    // turning every feature on everywhere:
    // - facetSearch is only needed where the UI calls the facet-search
    //   endpoint (domain, author — see web/src/lib/meili.ts).
    // - comparison (<, >, >=, <=) is only needed for the numeric range
    //   filters the UI actually issues (points, created_at); everything else
    //   only ever uses equality (=, !=, EXISTS).
    json!({
        "searchableAttributes": ["title", "text"],
        "filterableAttributes": [
            { "attributePatterns": ["domain", "author"],
              "features": { "facetSearch": true, "filter": { "equality": true, "comparison": false } } },
            { "attributePatterns": ["type", "tags", "url"],
              "features": { "facetSearch": false, "filter": { "equality": true, "comparison": false } } },
            { "attributePatterns": ["enrich_gen"],
              "features": { "facetSearch": false, "filter": { "equality": true, "comparison": true } } },
            { "attributePatterns": ["points", "num_comments", "created_at"],
              "features": { "facetSearch": false, "filter": { "equality": false, "comparison": true } } }
        ],
        "sortableAttributes": ["created_at", "points", "num_comments"],
        // sort sits BEFORE attribute (default is after): when a query asks
        // for an explicit sort — the UI's Newest/Points modes and the
        // ghost-completion query's points:desc — it should dominate over
        // which attribute/position the terms matched in. Queries without a
        // sort param are unaffected.
        "rankingRules": [
            "words", "typo", "proximity", "sort", "attribute", "exactness",
            "points:desc"
        ],
        // Terms only need to share an attribute, not sit at an exact word
        // distance — cheaper to compute and title/text are independent
        // fields anyway, so exact cross-field distance was never meaningful.
        "proximityPrecision": "byAttribute",
        "faceting": { "maxValuesPerFacet": 100 },
        "pagination": { "maxTotalHits": 10000 },
        "typoTolerance": { "minWordSizeForTypos": { "oneTypo": 4, "twoTypos": 9 } }
    })
}

/// Search configuration of [`COMMENTS_INDEX`] — only what the Comments tab
/// and the thread view actually query. HN exposes no comment scores, and
/// comments carry no tags, url or domain worth filtering on, so none of those
/// are filterable: at ~40M documents every facet dropped is indexing work
/// saved on each write.
fn comments_settings() -> serde_json::Value {
    json!({
        "searchableAttributes": ["text"],
        "filterableAttributes": [
            { "attributePatterns": ["author"],
              "features": { "facetSearch": true, "filter": { "equality": true, "comparison": false } } },
            // The thread walk's `parent IN [...]`.
            { "attributePatterns": ["parent"],
              "features": { "facetSearch": false, "filter": { "equality": true, "comparison": false } } },
            { "attributePatterns": ["created_at"],
              "features": { "facetSearch": false, "filter": { "equality": false, "comparison": true } } }
        ],
        "sortableAttributes": ["created_at"],
        "rankingRules": ["words", "typo", "proximity", "sort", "attribute", "exactness"],
        "proximityPrecision": "byAttribute",
        "faceting": { "maxValuesPerFacet": 100 },
        "pagination": { "maxTotalHits": 10000 },
        "typoTolerance": { "minWordSizeForTypos": { "oneTypo": 4, "twoTypos": 9 } },
        // Comments are never embedded. Explicit so that the index `split`
        // renames from the pre-split `hn` drops the embedder it carried.
        "embedders": null
    })
}

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

    /// Whether an index exists. Any answer other than 200/404 is an error.
    pub async fn index_exists(&self, uid: &str) -> Result<bool> {
        let resp = self
            .request(reqwest::Method::GET, &format!("/indexes/{uid}"))
            .send()
            .await?;
        match resp.status() {
            s if s.is_success() => Ok(true),
            reqwest::StatusCode::NOT_FOUND => Ok(false),
            status => {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("checking index '{uid}': {status}: {body}")
            }
        }
    }

    /// Refuse to touch the split indexes while the pre-split `hn` index is
    /// still around. Otherwise a new indexer deployed before the migration
    /// would create empty `hn-stories`/`hn-comments` indexes and start
    /// syncing into them — and `split` could then no longer rename `hn`.
    pub async fn refuse_legacy(&self) -> Result<()> {
        if self.index_exists(LEGACY_INDEX).await? {
            anyhow::bail!(
                "the pre-split index '{LEGACY_INDEX}' still exists — run `hn-indexer split` \
                 to move it into '{STORIES_INDEX}' and '{COMMENTS_INDEX}' first"
            );
        }
        Ok(())
    }

    /// Create and configure each index only when it doesn't exist yet.
    /// Existing indexes are left completely untouched — no settings task,
    /// no reindex, no startup stall. Settings changes are pushed explicitly
    /// with the `settings` command.
    pub async fn ensure_indexes(&self) -> Result<()> {
        self.refuse_legacy().await?;
        for uid in [STORIES_INDEX, COMMENTS_INDEX] {
            if !self.index_exists(uid).await? {
                tracing::info!("index '{uid}' does not exist — creating and configuring it");
                if let Some(task) = self.configure_index(uid).await? {
                    self.wait_for_task(task).await?;
                }
            }
        }
        Ok(())
    }

    /// Create both indexes (idempotent) and apply their search configuration,
    /// waiting until it is live.
    pub async fn apply_settings(&self) -> Result<()> {
        self.refuse_legacy().await?;
        for uid in [STORIES_INDEX, COMMENTS_INDEX] {
            // Settings are applied asynchronously; later calls (filters on
            // the newly filterable attributes) need them to actually be live.
            if let Some(task) = self.configure_index(uid).await? {
                self.wait_for_task(task).await?;
            }
        }
        Ok(())
    }

    /// Create `uid` if needed and submit its settings. Returns the settings
    /// task without waiting on it: on a populated index it is a full reindex.
    pub async fn configure_index(&self, uid: &str) -> Result<Option<u64>> {
        // Index creation is a task; if the index already exists the task
        // fails asynchronously, which is fine — settings below still apply.
        self.request(reqwest::Method::POST, "/indexes")
            .json(&json!({ "uid": uid, "primaryKey": "id" }))
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("creating index '{uid}'"))?;
        let settings = if uid == COMMENTS_INDEX {
            comments_settings()
        } else {
            stories_settings()
        };
        let task: serde_json::Value = self
            .request(reqwest::Method::PATCH, &format!("/indexes/{uid}/settings"))
            .json(&settings)
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("applying settings to '{uid}'"))?
            .json()
            .await?;
        Ok(task["taskUid"].as_u64())
    }

    /// Upsert documents. Uses PUT (add-or-UPDATE) rather than POST
    /// (add-or-replace) so re-syncing an item never wipes fields the item
    /// payload doesn't carry — notably the enrichment `content` field.
    /// Returns the task uid.
    pub async fn add_documents<T: serde::Serialize>(
        &self,
        index: &str,
        docs: &[T],
    ) -> Result<Option<u64>> {
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
                    &format!("/indexes/{index}/documents?primaryKey=id"),
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

    /// One page of full documents matching `filter`, plus the total number
    /// of matches. Vectors are not retrieved.
    pub async fn fetch_documents(
        &self,
        index: &str,
        filter: &str,
        offset: u64,
        limit: usize,
    ) -> Result<(Vec<serde_json::Value>, u64)> {
        let resp: serde_json::Value = self
            .request(
                reqwest::Method::POST,
                &format!("/indexes/{index}/documents/fetch"),
            )
            .json(&json!({ "filter": filter, "offset": offset, "limit": limit }))
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("fetching documents from '{index}'"))?
            .json()
            .await?;
        let total = resp["total"].as_u64().unwrap_or(0);
        let docs = resp["results"].as_array().cloned().unwrap_or_default();
        Ok((docs, total))
    }

    /// Delete every document matching `filter`. Returns the task uid.
    pub async fn delete_by_filter(&self, index: &str, filter: &str) -> Result<Option<u64>> {
        let task: serde_json::Value = self
            .request(
                reqwest::Method::POST,
                &format!("/indexes/{index}/documents/delete"),
            )
            .json(&json!({ "filter": filter }))
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("deleting documents from '{index}'"))?
            .json()
            .await?;
        Ok(task["taskUid"].as_u64())
    }

    /// Rename an index in place (Meilisearch ≥ 1.18). Its documents,
    /// settings and internal databases are kept as they are — nothing is
    /// reindexed. Returns the task uid.
    pub async fn rename_index(&self, from: &str, to: &str) -> Result<Option<u64>> {
        let task: serde_json::Value = self
            .request(reqwest::Method::PATCH, &format!("/indexes/{from}"))
            .json(&json!({ "uid": to }))
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("renaming index '{from}' to '{to}'"))?
            .json()
            .await?;
        Ok(task["taskUid"].as_u64())
    }

    /// Names of the embedders configured on an index.
    pub async fn embedder_names(&self, index: &str) -> Result<Vec<String>> {
        let embedders: serde_json::Value = self
            .request(
                reqwest::Method::GET,
                &format!("/indexes/{index}/settings/embedders"),
            )
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("reading embedders of '{index}'"))?
            .json()
            .await?;
        Ok(embedders
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default())
    }

    /// Fetch (id, url) pairs of documents that still need enrichment:
    /// link stories stamped with an older `enrich_gen` than the current one,
    /// optionally limited to those created at or after `since` (unix secs).
    ///
    /// Only `type = "story"`: job posts do link
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
    ) -> Result<Vec<Enrichable>> {
        let mut filter = format!(
            "type = \"story\" AND url EXISTS \
             AND (enrich_gen NOT EXISTS OR enrich_gen < {ENRICH_GENERATION})"
        );
        if let Some(since) = since {
            filter.push_str(&format!(" AND created_at >= {since}"));
        }
        let body = json!({
            "filter": filter,
            "fields": ["id", "url", "title"],
            "limit": limit,
        });
        let resp: serde_json::Value = self
            .request(
                reqwest::Method::POST,
                &format!("/indexes/{STORIES_INDEX}/documents/fetch"),
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
            .filter_map(|doc| {
                Some(Enrichable {
                    id: doc["id"].as_u64()?,
                    url: doc["url"].as_str()?.to_string(),
                    title: doc["title"].as_str().map(str::to_string),
                })
            })
            .collect())
    }

    /// Configure the `default` embedder used for semantic/hybrid search, on
    /// [`STORIES_INDEX`] only — comments are never embedded.
    ///
    /// Touches only the `embedders` setting (plus the `multimodal`
    /// experimental feature that fragments need), never the rest of the
    /// index settings — so it is safe on an index where `apply_settings()`
    /// would trigger a full reindex.
    ///
    /// See [`ARTICLE_FRAGMENT`] for how untitled poll options are kept out
    /// and why it has to be done the way it is.
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
            // which embeds every document — untitled poll options as "".
            "huggingface" => anyhow::bail!(
                "the local huggingFace embedder cannot skip untitled documents \
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
                &format!("/indexes/{STORIES_INDEX}/settings"),
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

    pub async fn document_count(&self, index: &str) -> Result<u64> {
        let stats: serde_json::Value = self
            .request(reqwest::Method::GET, &format!("/indexes/{index}/stats"))
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

    /// Skipping untitled documents depends on the fragment OUTPUTTING
    /// `doc.title` unconditionally — see ARTICLE_FRAGMENT. An `{% if %}`
    /// guard would make Meilisearch embed them as "" instead of skipping them.
    #[test]
    fn article_fragment_gates_on_title() {
        assert!(ARTICLE_FRAGMENT.starts_with("{{ doc.title }}"));
        assert!(
            !ARTICLE_FRAGMENT.contains("doc.type"),
            "a type-based guard embeds documents as empty strings rather than skipping them"
        );
        assert!(
            !ARTICLE_FRAGMENT.contains("doc.text"),
            "poll-option and Ask HN body text is not embedded"
        );
    }

    #[test]
    fn routes_items_by_type() {
        assert_eq!(index_for("comment"), COMMENTS_INDEX);
        for kind in ["story", "job", "poll", "pollopt"] {
            assert_eq!(index_for(kind), STORIES_INDEX, "{kind}");
        }
    }

    /// Everything the web app filters, facets or sorts on must be declared
    /// on the index it queries, or Meilisearch rejects the search.
    #[test]
    fn settings_cover_what_the_ui_queries() {
        let declared = |settings: serde_json::Value| -> Vec<String> {
            settings["filterableAttributes"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|rule| rule["attributePatterns"].as_array().unwrap().clone())
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        let stories = declared(stories_settings());
        for attr in [
            "tags",
            "domain",
            "author",
            "points",
            "created_at",
            "type",
            "url",
            "enrich_gen",
        ] {
            assert!(stories.contains(&attr.to_string()), "stories: {attr}");
        }
        let comments = declared(comments_settings());
        for attr in ["author", "parent", "created_at"] {
            assert!(comments.contains(&attr.to_string()), "comments: {attr}");
        }
        assert!(comments_settings()["embedders"].is_null());
    }
}
