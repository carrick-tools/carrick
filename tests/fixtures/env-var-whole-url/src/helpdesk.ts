// The whole request URL comes from an environment variable, with a local
// default. The call site passes the binding straight through, so it states no
// path: the only path this request has is inside the fallback literal.
const url = process.env.HELPDESK_URL ?? "http://localhost:7100/api/answer";

export async function askHelpdesk(question: string): Promise<string> {
  const res = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ question }),
  });
  return res.text();
}

// A base URL with the path at the call site: the shape that already resolves,
// kept here so the whole-URL rule is shown not to disturb it.
const base = process.env.CATALOG_URL ?? "http://localhost:4001";

export async function listItems(): Promise<string> {
  const res = await fetch(`${base}/api/v1/items`, { method: "GET" });
  return res.text();
}
