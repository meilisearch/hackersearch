import type { HNHit } from "./meili";

/**
 * Inline-completion for the search box: given the current query and the
 * hits already on screen, find the first word in the top results that
 * extends the word being typed, and return the missing suffix.
 * "Mei" + a hit titled "Meilisearch 1.6 released" -> "lisearch".
 */
export function findCompletion(q: string, hits: HNHit[]): string {
  // Nothing to complete after a finished word.
  if (!q || /\s$/.test(q)) return "";
  const prefix = q.split(/\s+/).pop() ?? "";
  if (prefix.length < 2) return "";
  const lower = prefix.toLowerCase();

  for (const hit of hits.slice(0, 8)) {
    for (const source of [hit.title, hit.text]) {
      if (!source) continue;
      for (const raw of source.split(/\s+/)) {
        // Trim surrounding punctuation ("(Meilisearch," -> "Meilisearch").
        const word = raw.replace(/^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$/gu, "");
        if (word.length > prefix.length && word.toLowerCase().startsWith(lower)) {
          return word.slice(prefix.length);
        }
      }
    }
  }
  return "";
}
