use anyhow::{Context, Result};
use serde_json::json;

pub const INDEX_UID: &str = "hn";

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

    /// Fetch (id, url) pairs of documents that still need enrichment.
    pub async fn fetch_enrichable(&self, limit: usize) -> Result<Vec<(u64, String)>> {
        let body = json!({
            "filter": "url EXISTS AND enriched NOT EXISTS AND type != \"comment\"",
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
    /// The document template prefers the enriched article content over the
    /// item's own text, mirroring the hackerverse approach.
    pub async fn apply_embedder(&self, kind: &str) -> Result<()> {
        let template = "{{ doc.type }}: {% if doc.title %}{{ doc.title }}\n{% endif %}\
            {% if doc.content %}{{ doc.content | truncatewords: 400 }}\
            {% elsif doc.text %}{{ doc.text | truncatewords: 200 }}{% endif %}";
        let embedder = match kind {
            "huggingface" => json!({
                "source": "huggingFace",
                "documentTemplate": template,
            }),
            "openai" => json!({
                "source": "openAi",
                "model": "text-embedding-3-small",
                "apiKey": std::env::var("OPENAI_API_KEY").unwrap_or_default(),
                "documentTemplate": template,
            }),
            other => anyhow::bail!("unknown embedder '{other}' (huggingface|openai)"),
        };
        self.request(
            reqwest::Method::PATCH,
            &format!("/indexes/{INDEX_UID}/settings"),
        )
        .json(&json!({ "embedders": { "default": embedder } }))
        .send()
        .await?
        .error_for_status()
        .context("applying embedder settings")?;
        Ok(())
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
