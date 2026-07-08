import { HL_END, HL_START } from "@/lib/meili";

/**
 * Render a Meilisearch-highlighted string as React nodes. Matches are
 * delimited by private-use-area markers, so no HTML ever gets injected.
 */
export function Highlighted({ text }: { text: string }) {
  if (!text.includes(HL_START)) return <>{text}</>;
  const nodes: React.ReactNode[] = [];
  text.split(HL_START).forEach((chunk, i) => {
    if (i === 0) {
      if (chunk) nodes.push(chunk);
      return;
    }
    const end = chunk.indexOf(HL_END);
    if (end === -1) {
      nodes.push(chunk);
      return;
    }
    const match = chunk.slice(0, end);
    const rest = chunk.slice(end + HL_END.length);
    if (match) nodes.push(<mark key={i}>{match}</mark>);
    if (rest) nodes.push(rest);
  });
  return <>{nodes}</>;
}
