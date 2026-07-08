//! Article-content enrichment, hackerverse-style: fetch the page a story
//! links to, strip semantically non-primary HTML, and keep the main text.
//! The result is stored on the document for embedding generation only —
//! it is deliberately NOT part of the searchable attributes.

use anyhow::Result;
use scraper::{ElementRef, Html, Selector};

const MAX_BODY_BYTES: usize = 2_000_000;

/// Credentials for Cloudflare's Browser Rendering markdown endpoint —
/// a real headless browser, so JS-rendered pages extract correctly.
#[derive(Clone)]
pub struct Cloudflare {
    pub account_id: String,
    pub token: String,
}

impl Cloudflare {
    /// Built from CLOUDFLARE_ACCOUNT_ID / CLOUDFLARE_API_TOKEN when both set.
    pub fn from_env() -> Option<Self> {
        Some(Self {
            account_id: std::env::var("CLOUDFLARE_ACCOUNT_ID").ok()?,
            token: std::env::var("CLOUDFLARE_API_TOKEN").ok()?,
        })
    }
}

/// Render a page in Cloudflare's headless browser and get it back as
/// markdown. Returns None on failures (caller falls back to local fetch).
pub async fn markdown_via_cloudflare(
    client: &reqwest::Client,
    cf: &Cloudflare,
    url: &str,
    max_chars: usize,
) -> Result<Option<String>> {
    let endpoint = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/browser-rendering/markdown",
        cf.account_id
    );
    let mut delay = std::time::Duration::from_secs(2);
    for _ in 0..4 {
        let resp = client
            .post(&endpoint)
            .bearer_auth(&cf.token)
            .json(&serde_json::json!({
                "url": url,
                "gotoOptions": { "waitUntil": "networkidle2" },
            }))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2);
            continue;
        }
        if !resp.status().is_success() {
            return Ok(None);
        }
        let body: serde_json::Value = resp.json().await?;
        if body["success"].as_bool() != Some(true) {
            return Ok(None);
        }
        return Ok(body["result"]
            .as_str()
            .map(|md| truncate_chars(&clean_markdown(md), max_chars))
            .filter(|text| text.chars().count() >= 80));
    }
    Ok(None)
}

/// Reduce markdown to embedding-friendly prose: keep link text, drop link
/// targets and images — URLs are noise in an embedding.
fn clean_markdown(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut chars = md.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '!' if chars.peek() == Some(&'[') => {
                // Image: skip "![alt](src)" entirely.
                skip_bracket_pair(&mut chars);
            }
            '[' => {
                // Link: keep the text, drop "(target)".
                let mut text = String::new();
                for inner in chars.by_ref() {
                    if inner == ']' {
                        break;
                    }
                    text.push(inner);
                }
                out.push_str(&text);
                if chars.peek() == Some(&'(') {
                    for inner in chars.by_ref() {
                        if inner == ')' {
                            break;
                        }
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out.lines()
        .map(|line| line.trim_start_matches(['#', '>', '*', '-', ' ']).trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Consume "…[…](…)" from a char stream (used to skip images).
fn skip_bracket_pair(chars: &mut std::iter::Peekable<std::str::Chars>) {
    if chars.peek() == Some(&'[') {
        for c in chars.by_ref() {
            if c == ']' {
                break;
            }
        }
    }
    if chars.peek() == Some(&'(') {
        for c in chars.by_ref() {
            if c == ')' {
                break;
            }
        }
    }
}

/// Elements whose subtree never contains primary content.
const STRIP_TAGS: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "canvas", "head", "nav", "header", "footer",
    "aside", "form", "iframe", "button", "select", "input", "label", "menu", "dialog",
];

/// Tags that imply a line break between text runs.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "br",
    "div",
    "section",
    "article",
    "li",
    "ul",
    "ol",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "tr",
    "table",
    "blockquote",
    "pre",
    "figure",
    "hr",
    "dd",
    "dt",
];

/// Fetch a page and return its raw HTML, or None for non-HTML content,
/// error statuses, or oversized bodies (truncated at MAX_BODY_BYTES).
pub async fn fetch_page(client: &reqwest::Client, url: &str) -> Result<Option<String>> {
    let mut resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.is_empty() && !content_type.contains("html") {
        return Ok(None);
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY_BYTES {
            break;
        }
    }
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

/// Extract the main article text from an HTML document. Prefers semantic
/// containers (<article>, then <main>) when they hold enough text, falling
/// back to <body>. Returns None when nothing substantial remains.
pub fn extract_content(html: &str, max_chars: usize) -> Option<String> {
    let doc = Html::parse_document(html);
    let mut fallback: Option<String> = None;
    for tag in ["article", "main", "body"] {
        let selector = Selector::parse(tag).expect("static selector");
        if let Some(root) = doc.select(&selector).next() {
            let text = normalize(&collect_text(root));
            if text.chars().count() >= 400 {
                return Some(truncate_chars(&text, max_chars));
            }
            if fallback.is_none() && !text.is_empty() {
                fallback = Some(text);
            }
        }
    }
    fallback
        .filter(|t| t.chars().count() >= 80)
        .map(|t| truncate_chars(&t, max_chars))
}

fn collect_text(root: ElementRef) -> String {
    let mut out = String::new();
    walk(root, &mut out);
    out
}

fn walk(element: ElementRef, out: &mut String) {
    let tag = element.value().name();
    if STRIP_TAGS.contains(&tag) {
        return;
    }
    for child in element.children() {
        match child.value() {
            scraper::Node::Text(text) => out.push_str(text),
            scraper::Node::Element(_) => {
                if let Some(el) = ElementRef::wrap(child) {
                    walk(el, out);
                }
            }
            _ => {}
        }
    }
    if BLOCK_TAGS.contains(&tag) {
        out.push('\n');
    }
}

/// Collapse intra-line whitespace and drop empty lines.
fn normalize(raw: &str) -> String {
    raw.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_article_and_strips_chrome() {
        let html = r#"<html><head><title>t</title><script>x()</script></head>
        <body>
          <nav>Home About Pricing</nav>
          <article><h1>Real title</h1><p>First paragraph of the story.</p>
          <p>Second paragraph with more words to cross the minimum threshold
          for semantic containers, padded out with additional prose so the
          extractor treats this as the primary content of the page rather
          than falling back to the body. Even more filler text here to be
          safe, because four hundred characters is a decent chunk of prose
          when you actually have to type it out by hand in a unit test.</p></article>
          <footer>Copyright</footer>
        </body></html>"#;
        let content = extract_content(html, 4000).expect("content");
        assert!(content.starts_with("Real title"));
        assert!(content.contains("Second paragraph"));
        assert!(!content.contains("Pricing"));
        assert!(!content.contains("Copyright"));
        assert!(!content.contains("x()"));
    }

    #[test]
    fn small_pages_fall_back_to_body_or_nothing() {
        assert_eq!(extract_content("<body><p>tiny</p></body>", 4000), None);
        let body = "<body><p>This body has no article tag but does have enough
        text to be worth keeping around as a fallback, comfortably past the
        eighty character floor.</p></body>";
        assert!(extract_content(body, 4000).is_some());
    }

    #[test]
    fn cleans_markdown_for_embeddings() {
        let md = "# Title\n\nSome [linked text](https://example.com/x) here.\n\n![diagram](https://img.example.com/d.png)\n\n> quoted *emphasis*\n";
        let cleaned = clean_markdown(md);
        assert!(cleaned.contains("Title"));
        assert!(cleaned.contains("linked text here."));
        assert!(!cleaned.contains("example.com"));
        assert!(!cleaned.contains("!["));
        assert!(cleaned.contains("quoted"));
    }

    #[test]
    fn truncates_to_char_budget() {
        let html = format!("<body><p>{}</p></body>", "word ".repeat(500));
        let content = extract_content(&html, 100).expect("content");
        assert_eq!(content.chars().count(), 100);
    }
}
