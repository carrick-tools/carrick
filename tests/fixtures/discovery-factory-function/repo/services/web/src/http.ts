const BASE = "https://api.example.test";

export interface Send {
  (method: string, path: string, body?: unknown): Promise<unknown>;
}

export function makeClient(options: { token: string; fetch?: typeof globalThis.fetch }): Send {
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  async function request(method: string, path: string, body?: unknown) {
    const res = await doFetch(`${BASE}${path}`, { method, body: JSON.stringify(body) });
    return res.json();
  }
  return request as Send;
}
