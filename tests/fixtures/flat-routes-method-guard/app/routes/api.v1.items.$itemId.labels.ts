// The mirror case: a destructured local case-folded down rather than up, with
// a lowercase literal to match. The module serves GET and nothing else.

type Labels = { labels: string[] };

export async function action({ request }: { request: Request }): Promise<Labels> {
  const { method } = request;
  if (method.toLowerCase() === "get") {
    return { labels: [] };
  }
  throw new Response("Method Not Allowed", { status: 405 });
}
