const BASE = "https://api.example.test";

export interface BoundSend {
  (init: { path: string; fetch?: typeof globalThis.fetch }): Promise<unknown>;
}

export function makeBound(options: { fetch: (url: string) => Promise<Response> }): BoundSend {
  const doFetch = options.fetch.bind(options);
  async function request(init: { path: string; fetch?: typeof globalThis.fetch }) {
    const res = await doFetch(`${BASE}${init.path}`);
    return res.json();
  }
  return request as BoundSend;
}
