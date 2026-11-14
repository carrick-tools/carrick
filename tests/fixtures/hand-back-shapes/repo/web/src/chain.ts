import type { Reply } from './types';

const BASE = 'http://localhost:4100';

async function retry<T>(run: () => Promise<T>): Promise<T> {
  return run();
}

function handle(res: Response) {
  return { ok: res.ok, code: res.status };
}

export async function thenParse(url: string) {
  return fetch(new URL(url, BASE)).then((r) => r.json() as Promise<Reply>);
}

export async function thenHold(url: string) {
  return fetch(new URL(url, BASE)).then(async (res) => ({ res, json: (await res.json()) as Reply }));
}

export async function catchParse(url: string) {
  return fetch(new URL(url, BASE))
    .then((r) => r.json() as Promise<Reply>)
    .catch(() => null);
}

export async function retryFetch(url: string) {
  return retry(() => fetch(new URL(url, BASE)));
}

export async function handled(url: string) {
  const res = await fetch(new URL(url, BASE));
  return handle(res);
}
