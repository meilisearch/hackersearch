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
    /// Built from CLOUDFLARE_ACCOUNT_ID / CLOUDFLARE_API_TOKEN when both are
    /// set to non-empty values. Empty counts as absent: compose and shell
    /// exports both happily define a variable as "", and blank credentials
    /// would otherwise 401 on every page before falling back to local.
    pub fn from_env() -> Option<Self> {
        fn non_empty(key: &str) -> Option<String> {
            std::env::var(key).ok().filter(|v| !v.trim().is_empty())
        }
        Some(Self {
            account_id: non_empty("CLOUDFLARE_ACCOUNT_ID")?,
            token: non_empty("CLOUDFLARE_API_TOKEN")?,
        })
    }
}

/// How one Cloudflare render ended. Kept distinct so the crawler can report
/// *why* it is slow or failing, not merely that it is.
#[derive(Debug)]
pub enum CfOutcome {
    /// Rendered, and enough text survived cleaning.
    Content(String),
    /// Rendered (or `success: false`), but nothing usable came back.
    Empty,
    /// Still 429 after every retry.
    Throttled,
    /// Any other non-2xx status from the API.
    Http(u16),
    /// Our request ran past the client timeout — a render that never settled.
    Timeout,
    /// Connection-level failure.
    Transport,
}

/// One Cloudflare attempt, with what it cost.
pub struct CfAttempt {
    pub outcome: CfOutcome,
    /// 429 responses received; each one cost a backoff sleep.
    pub throttled_retries: u32,
    pub elapsed: std::time::Duration,
}

/// Render a page in Cloudflare's headless browser and get it back as
/// markdown. Never errors: every way it can end is reported in the outcome,
/// and anything but `Content` makes the caller fall back to a local fetch.
pub async fn markdown_via_cloudflare(
    client: &reqwest::Client,
    cf: &Cloudflare,
    url: &str,
    max_chars: usize,
) -> CfAttempt {
    let started = std::time::Instant::now();
    let done = |outcome: CfOutcome, throttled_retries: u32| CfAttempt {
        outcome,
        throttled_retries,
        elapsed: started.elapsed(),
    };
    let endpoint = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/browser-rendering/markdown",
        cf.account_id
    );
    let mut delay = std::time::Duration::from_secs(2);
    let mut throttled_retries = 0;
    for _ in 0..4 {
        let sent = client
            .post(&endpoint)
            .bearer_auth(&cf.token)
            .json(&serde_json::json!({
                "url": url,
                "gotoOptions": { "waitUntil": "networkidle2" },
            }))
            .send()
            .await;
        let resp = match sent {
            Ok(resp) => resp,
            Err(e) if e.is_timeout() => return done(CfOutcome::Timeout, throttled_retries),
            Err(_) => return done(CfOutcome::Transport, throttled_retries),
        };
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            throttled_retries += 1;
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2);
            continue;
        }
        if !resp.status().is_success() {
            return done(CfOutcome::Http(resp.status().as_u16()), throttled_retries);
        }
        let body: serde_json::Value = match resp.json().await {
            Ok(body) => body,
            Err(e) if e.is_timeout() => return done(CfOutcome::Timeout, throttled_retries),
            Err(_) => return done(CfOutcome::Transport, throttled_retries),
        };
        if body["success"].as_bool() != Some(true) {
            return done(CfOutcome::Empty, throttled_retries);
        }
        let outcome = body["result"]
            .as_str()
            .map(|md| truncate_chars(&clean_markdown(md), max_chars))
            .filter(|text| text.chars().count() >= 80)
            .map_or(CfOutcome::Empty, CfOutcome::Content);
        return done(outcome, throttled_retries);
    }
    done(CfOutcome::Throttled, throttled_retries)
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

/// What a single plain fetch yields: the article text and the page's own
/// description (meta / Open Graph / JSON-LD), each independently optional.
pub struct Extracted {
    pub content: Option<String>,
    pub description: Option<String>,
}

/// Parse a page once and pull out both the article text and a
/// page-specific description. `title` is the HN title, used to discard
/// descriptions that merely repeat it.
pub fn extract_page(html: &str, max_chars: usize, title: Option<&str>) -> Extracted {
    let doc = Html::parse_document(html);
    Extracted {
        content: content_from(&doc, max_chars),
        description: description_from(&doc, title),
    }
}

/// Extract the main article text from an HTML document. Prefers semantic
/// containers (<article>, then <main>) when they hold enough text, falling
/// back to <body>. Returns None when nothing substantial remains.
#[cfg(test)]
pub fn extract_content(html: &str, max_chars: usize) -> Option<String> {
    content_from(&Html::parse_document(html), max_chars)
}

fn content_from(doc: &Html, max_chars: usize) -> Option<String> {
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

/// Descriptions shorter than this are almost never a real description of the
/// page ("Be honest.", "Home") — measured on a sample of HN submissions.
const MIN_DESCRIPTION_CHARS: usize = 20;
const MAX_DESCRIPTION_CHARS: usize = 500;

/// The page's own description, in order of how reliably each source is
/// written per page: `<meta name=description>`, Open Graph, Twitter card,
/// then JSON-LD `description` / `abstract`. Available without running any
/// JavaScript, which is what makes it free to collect on every story.
///
/// On a sample of 240 real HN links, ~61% had one that was page-specific and
/// a real sentence. They read like teasers rather than summaries, so this is
/// a complement to `content`, not a replacement for it.
fn description_from(doc: &Html, title: Option<&str>) -> Option<String> {
    let meta = |selector: &str| {
        let sel = Selector::parse(selector).expect("static selector");
        doc.select(&sel)
            .filter_map(|el| el.value().attr("content"))
            .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
            .find(|v| !v.is_empty())
    };
    let candidate = meta(r#"meta[name="description" i]"#)
        .or_else(|| meta(r#"meta[property="og:description" i]"#))
        .or_else(|| {
            meta(r#"meta[name="twitter:description" i], meta[property="twitter:description" i]"#)
        })
        .or_else(|| json_ld_description(doc))?;

    if candidate.chars().count() < MIN_DESCRIPTION_CHARS || repeats_title(&candidate, title) {
        return None;
    }
    Some(truncate_chars(&candidate, MAX_DESCRIPTION_CHARS))
}

fn json_ld_description(doc: &Html) -> Option<String> {
    fn walk(node: &serde_json::Value) -> Option<String> {
        match node {
            serde_json::Value::Array(items) => items.iter().find_map(walk),
            serde_json::Value::Object(map) => ["description", "abstract"]
                .iter()
                .find_map(|k| {
                    map.get(*k)?
                        .as_str()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                })
                .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
                .or_else(|| map.values().find_map(walk)),
            _ => None,
        }
    }
    let sel = Selector::parse(r#"script[type="application/ld+json"]"#).expect("static selector");
    doc.select(&sel)
        .filter_map(|el| serde_json::from_str::<serde_json::Value>(&el.inner_html()).ok())
        .find_map(|v| walk(&v))
}

/// A description that is just the title again adds nothing to embed or show.
fn repeats_title(description: &str, title: Option<&str>) -> bool {
    let Some(title) = title else { return false };
    let norm = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (d, t) = (norm(description), norm(title));
    if d.is_empty() || t.is_empty() {
        return false;
    }
    let prefix = |s: &str| s.chars().take(40).collect::<String>();
    d.starts_with(&prefix(&t)) || t.starts_with(&prefix(&d))
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
    fn description_prefers_meta_then_og_then_json_ld() {
        let meta = r#"<head><meta name="description" content="The meta description wins here.">
            <meta property="og:description" content="Open Graph loses."></head>"#;
        assert_eq!(
            extract_page(meta, 4000, None).description.as_deref(),
            Some("The meta description wins here.")
        );
        let og = r#"<head><meta property="og:description" content="Only   Open Graph,
            with messy whitespace."></head>"#;
        assert_eq!(
            extract_page(og, 4000, None).description.as_deref(),
            Some("Only Open Graph, with messy whitespace.")
        );
        let ld = r#"<script type="application/ld+json">
            {"@graph":[{"@type":"WebSite"},{"@type":"Article","description":"Nested JSON-LD description."}]}
            </script>"#;
        assert_eq!(
            extract_page(ld, 4000, None).description.as_deref(),
            Some("Nested JSON-LD description.")
        );
    }

    #[test]
    fn description_rejects_noise() {
        let short = r#"<meta name="description" content="Be honest.">"#;
        assert_eq!(extract_page(short, 4000, None).description, None);
        let echo = r#"<meta name="description" content="Explaining to business people why building software is still hard">"#;
        assert_eq!(
            extract_page(
                echo,
                4000,
                Some("Explaining to business people why building software is still hard")
            )
            .description,
            None
        );
        assert_eq!(
            extract_page("<p>no metadata at all</p>", 4000, None).description,
            None
        );
    }

    #[test]
    fn content_and_description_are_independent() {
        let html = r#"<html><head><meta name="description" content="A page-specific teaser sentence."></head>
            <body><nav>menu</nav></body></html>"#;
        let page = extract_page(html, 4000, None);
        assert_eq!(page.content, None, "a JS shell has no extractable text");
        assert!(
            page.description.is_some(),
            "but can still carry a description"
        );
    }

    #[test]
    fn truncates_to_char_budget() {
        let html = format!("<body><p>{}</p></body>", "word ".repeat(500));
        let content = extract_content(&html, 100).expect("content");
        assert_eq!(content.chars().count(), 100);
    }
}
