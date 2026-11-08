// Rung 1: an interface client whose members are arrows in the object a
// factory returns; each arrow sends with the URL its options carry.
export interface Client {
  get(opts: { url: string }): Promise<Response>;
  post(opts: { url: string; body?: unknown }): Promise<Response>;
}

export function createClient(): Client {
  return {
    get: (opts) => fetch(opts.url),
    post: (opts) => fetch(opts.url, { method: 'POST', body: JSON.stringify(opts.body) }),
  };
}

export const client = createClient();
