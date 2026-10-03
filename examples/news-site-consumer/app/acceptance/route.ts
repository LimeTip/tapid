export const dynamic = "force-dynamic";

export function GET() {
  return new Response("TAPID_NEWS_SITE_ACCEPTANCE_V1", {
    headers: { "content-type": "text/plain; charset=utf-8" },
  });
}
