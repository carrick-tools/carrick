import type { Reply } from './types';

const BASE = 'http://localhost:4100';

export async function call(url: string, init?: RequestInit) {
  const res = await fetch(new URL(url, BASE), init);
  return { res, json: (await res.json()) as Reply };
}

export async function callEither(url: string, early: boolean, init?: RequestInit) {
  const res = await fetch(new URL(url, BASE), init);
  if (early) {
    return { res, first: (await res.json()) as Reply };
  }
  return { res, second: (await res.json()) as Reply };
}

export async function callPlain(url: string) {
  const res = await fetch(new URL(url, BASE));
  return { res, json: (await res.json()) as Reply };
}

export async function callEitherPlain(url: string, early: boolean) {
  const res = await fetch(new URL(url, BASE));
  if (early) {
    return { res, first: (await res.json()) as Reply };
  }
  return { res, second: (await res.json()) as Reply };
}

export async function fetchText(url: string) {
  const res = await fetch(new URL(url, BASE));
  return { res, body: await res.text() };
}
