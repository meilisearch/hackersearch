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

        let settings = json!({
            "searchableAttributes": ["title", "text", "url", "domain", "author"],
            "filterableAttributes": [
                "type", "tags", "author", "domain",
                "points", "num_comments", "created_at", "parent",
                "url", "enriched"
            ],
            "sortableAttributes": ["created_at", "points", "num_comments"],
            "rankingRules": [
                "words", "typo", "proximity", "attribute", "sort", "exactness",
                "points:desc"
            ],
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
        let mut delay = std::time::Duration::from_millis(500);
        let mut last_err: Option<anyhow::Error> = None;
        for _ in 0..5 {
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
            delay = delay.saturating_mul(2);
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
