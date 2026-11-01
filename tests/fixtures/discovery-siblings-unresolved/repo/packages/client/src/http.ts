const BASE = "/v1";

async function send(url: string, init: { method: string; body?: string }) {
  const res = await fetch(url, init);
  return res.json();
}

export function getItem(id: string) {
  return send(`${BASE}/api/items/${id}`, { method: "GET" });
}

export function postTo(path: string, body: unknown) {
  return send(`${BASE}${path}`, { method: "POST", body: JSON.stringify(body) });
}

export class ItemsClient {
  remove(id: string) {
    return send(`${BASE}/api/items/${id}`, { method: "DELETE" });
  }
}

export function format(n: number): string {
  return n.toFixed(2);
}
