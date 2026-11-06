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

/// Turn HN's comment/self-post HTML into plain text, keeping what readers
/// need: paragraph breaks (`<p>` becomes a blank line), `<pre>` blocks
/// verbatim, and the FULL target of each link. HN renders long URLs as
/// truncated link text ("https://gist.github.com/smx-smx/a611...") with the
/// real URL only in `href`, so the text is dropped in favour of the href.
/// Good enough for search indexing and display — not a general HTML parser.
fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pre = false;
    // While inside <a href>, its text is skipped and the href emitted at </a>.
    let mut link: Option<String> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                let mut tag = String::new();
                for next in chars.by_ref() {
                    if next == '>' {
                        break;
                    }
                    tag.push(next);
                }
                let closing = tag.starts_with('/');
                let name = tag
                    .trim_start_matches('/')
                    .split(|ch: char| ch.is_whitespace())
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match name.as_str() {
                    "p" | "br" => paragraph_break(&mut out),
                    "pre" => {
                        paragraph_break(&mut out);
                        pre = !closing;
                    }
                    "a" if !closing => link = href(&tag),
                    "a" => {
                        if let Some(url) = link.take() {
                            push_text(&mut out, &url, pre);
                        }
                    }
                    _ => {}
                }
            }
            _ if link.is_some() => {}
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
                push_text(&mut out, decode_entity(&entity), pre);
            }
            _ => push_char(&mut out, c, pre),
        }
    }
    out.trim().to_string()
}

/// Append text, collapsing whitespace runs outside `<pre>`.
fn push_text(out: &mut String, text: &str, pre: bool) {
    for c in text.chars() {
        push_char(out, c, pre);
    }
}

fn push_char(out: &mut String, c: char, pre: bool) {
    if pre {
        out.push(c);
    } else if c.is_whitespace() {
        if !out.is_empty() && !out.ends_with(char::is_whitespace) {
            out.push(' ');
        }
    } else {
        out.push(c);
    }
}

/// End the current paragraph: exactly one blank line, never leading.
fn paragraph_break(out: &mut String) {
    let trimmed = out.trim_end_matches([' ', '\t']).len();
    out.truncate(trimmed);
    if out.is_empty() {
        return;
    }
    while !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// The decoded `href` of an `<a ...>` tag body, if it has one.
fn href(tag: &str) -> Option<String> {
    let start = tag.find("href=\"")? + "href=\"".len();
    let len = tag[start..].find('"')?;
    let mut url = String::new();
    // Entities inside attributes use the same handful HN emits in text.
    let mut chars = tag[start..start + len].chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            url.push(c);
            continue;
        }
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
        url.push_str(decode_entity(&entity));
    }
    Some(url).filter(|u| !u.is_empty())
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
        let html = "Hello <i>world</i> &amp; friends &#x27;quoted&#x27;";
        assert_eq!(strip_html(html), "Hello world & friends 'quoted'");
    }

    #[test]
    fn keeps_paragraph_breaks() {
        let html = "First   paragraph.<p>Second\nparagraph.<p><p>Third.";
        assert_eq!(strip_html(html), "First paragraph.\n\nSecond paragraph.\n\nThird.");
    }

    #[test]
    fn keeps_preformatted_blocks_verbatim() {
        let html = "Try:<p><pre><code>  fn main() {\n      run();\n  }\n</code></pre>Done.";
        assert_eq!(
            strip_html(html),
            "Try:\n\n  fn main() {\n      run();\n  }\n\nDone."
        );
    }

    #[test]
    fn replaces_truncated_link_text_with_full_href() {
        let html = "See <a href=\"https:&#x2F;&#x2F;gist.github.com&#x2F;q3k&#x2F;af3d93b6a1f399de28fe194add452d01\" rel=\"nofollow\">https:&#x2F;&#x2F;gist.github.com&#x2F;q3k&#x2F;af3d93b6a1f399de28fe1...</a> for details";
        assert_eq!(
            strip_html(html),
            "See https://gist.github.com/q3k/af3d93b6a1f399de28fe194add452d01 for details"
        );
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
