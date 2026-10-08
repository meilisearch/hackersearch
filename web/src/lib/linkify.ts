export type Segment = { kind: "text"; text: string } | { kind: "link"; url: string };

const URL_PATTERN = /https?:\/\/[^\s<>"]+/g;
// Sentence punctuation that ends up glued to a URL in prose.
const TRAILING = /[.,;:!?'"]+$/;

/**
 * Split plain comment text into text and link segments.
 *
 * Comments indexed before the indexer kept link targets carry HN's DISPLAY
 * text instead — the URL cut short and suffixed with "..." — which would be
 * a broken link, so those stay plain text.
 */
export function linkify(text: string): Segment[] {
  const segments: Segment[] = [];
  let last = 0;
  const pushText = (value: string) => {
    if (!value) return;
    const prev = segments[segments.length - 1];
    if (prev?.kind === "text") prev.text += value;
    else segments.push({ kind: "text", text: value });
  };

  for (const match of text.matchAll(URL_PATTERN)) {
    const start = match.index;
    let url = match[0];
    if (url.endsWith("...")) continue; // truncated display text, not a target
    url = url.replace(TRAILING, "");
    // Keep closing parens only as far as the URL opened them (Wikipedia
    // links); the rest belong to the prose around it.
    const count = (ch: string) => url.split(ch).length - 1;
    while (url.endsWith(")") && count(")") > count("(")) url = url.slice(0, -1);
    pushText(text.slice(last, start));
    segments.push({ kind: "link", url });
    last = start + url.length;
  }
  pushText(text.slice(last));
  return segments;
}
