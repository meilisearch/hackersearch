use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const API_BASE: &str = "https://hacker-news.firebaseio.com/v0";

/// Raw item as returned by the HN Firebase API.
#[derive(Debug, Deserialize)]
pub struct RawItem {
    pub id: u64,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub by: Option<String>,
    pub time: Option<i64>,
    pub text: Option<String>,
    pub url: Option<String>,
    pub title: Option<String>,
    pub score: Option<i64>,
    pub descendants: Option<i64>,
    pub parent: Option<u64>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub dead: bool,
}

#[derive(Debug, Deserialize)]
pub struct Updates {
    #[serde(default)]
    pub items: Vec<u64>,
}

/// Document shape stored in Meilisearch.
#[derive(Debug, Serialize)]
pub struct Doc {
    pub id: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    pub author: String,
    pub points: i64,
    pub num_comments: i64,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<u64>,
}

pub async fn max_item(client: &reqwest::Client) -> Result<u64> {
    let id: u64 = client
        .get(format!("{API_BASE}/maxitem.json"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("parsing maxitem")?;
    Ok(id)
}

pub async fn updated_items(client: &reqwest::Client) -> Result<Vec<u64>> {
    let updates: Updates = client
        .get(format!("{API_BASE}/updates.json"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .context("parsing updates")?;
    Ok(updates.items)
}

/// Fetch a single item, retrying on transient failures. Returns None for
/// ids the API knows nothing about (the endpoint returns literal `null`).
pub async fn fetch_item(client: &reqwest::Client, id: u64) -> Result<Option<RawItem>> {
    let url = format!("{API_BASE}/item/{id}.json");
    let mut delay = std::time::Duration::from_millis(400);
    let mut last_err: Option<anyhow::Error> = None;
    for _ in 0..5 {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                return resp
                    .json::<Option<RawItem>>()
                    .await
                    .context("decoding item");
            }
            Ok(resp) => last_err = Some(anyhow::anyhow!("status {} for item {id}", resp.status())),
            Err(e) => last_err = Some(e.into()),
        }
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2);
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("item {id}: retries exhausted")))
}

/// Convert a raw item into a search document. Returns None for deleted,
/// dead, or malformed items — they are not worth indexing.
pub fn to_doc(raw: RawItem) -> Option<Doc> {
    if raw.deleted || raw.dead {
        return None;
    }
    let kind = raw.kind?;
    let title = raw.title.filter(|t| !t.is_empty());
    let text = raw
        .text
        .as_deref()
        .map(strip_html)
        .filter(|t| !t.is_empty());
    let domain = raw.url.as_deref().and_then(extract_domain);

    let mut tags = vec![kind.clone()];
    if let Some(t) = &title {
        let lower = t.to_lowercase();
        if lower.starts_with("ask hn") {
            tags.push("ask_hn".into());
        } else if lower.starts_with("show hn") {
            tags.push("show_hn".into());
        } else if lower.starts_with("launch hn") {
            tags.push("launch_hn".into());
        } else if lower.starts_with("tell hn") {
            tags.push("tell_hn".into());
        }
    }

    Some(Doc {
        id: raw.id,
        kind,
        tags,
        title,
        text,
        url: raw.url,
        domain,
        author: raw.by.unwrap_or_else(|| "unknown".into()),
        points: raw.score.unwrap_or(0),
        num_comments: raw.descendants.unwrap_or(0),
        created_at: raw.time.unwrap_or(0),
        parent: raw.parent,
    })
}

fn extract_domain(raw_url: &str) -> Option<String> {
    let parsed = url::Url::parse(raw_url).ok()?;
    let host = parsed.host_str()?.to_lowercase();
    Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
}

/// Strip HTML tags and decode the handful of entities HN actually emits.
/// Good enough for search indexing — not a general-purpose HTML parser.
fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                in_tag = true;
                // Block-ish tags become whitespace so words don't glue together.
                if !out.ends_with(' ') && !out.is_empty() {
                    out.push(' ');
                }
            }
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            '&' => {
                let mut entity = String::new();
                while let Some(&next) = chars.peek() {
                    if next == ';' || entity.len() > 8 {
                        break;
                    }
                    entity.push(next);
                    chars.next();
                }
                if chars.peek() == Some(&';') {
                    chars.next();
                }
                out.push_str(decode_entity(&entity));
            }
            _ => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entity(entity: &str) -> &'static str {
    match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "#x27" | "#39" | "apos" => "'",
        "#x2F" | "#47" => "/",
        "nbsp" => " ",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        _ => " ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_and_entities() {
        let html = "Hello <p>world &amp; friends</p> <a href=\"x\">link</a> &#x27;quoted&#x27;";
        assert_eq!(strip_html(html), "Hello world & friends link 'quoted'");
    }

    #[test]
    fn extracts_domain() {
        assert_eq!(
            extract_domain("https://www.example.com/a/b?c=d"),
            Some("example.com".into())
        );
        assert_eq!(extract_domain("not a url"), None);
    }
}
