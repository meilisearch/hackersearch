import { FRONT_PAGE_REQUEST } from "@/lib/front-page";
import { searchHN } from "@/lib/meili";

// Rendered per request, but the CDN keeps each response for a minute and
// serves it stale while refreshing — so Meilisearch runs the expensive
// front-page search about once a minute in total, not once per visitor.
// Dynamic (rather than prerendered) so builds never depend on reaching the
// search instance.
export const dynamic = "force-dynamic";

export async function GET() {
  try {
    const result = await searchHN(FRONT_PAGE_REQUEST);
    return Response.json(result, {
      headers: {
        "Cache-Control": "public, s-maxage=60, stale-while-revalidate=600",
      },
    });
  } catch {
    // The client falls back to searching Meilisearch directly.
    return new Response(null, { status: 502, headers: { "Cache-Control": "no-store" } });
  }
}
