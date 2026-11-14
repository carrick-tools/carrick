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

export async function probeFetchWhole(url: string) {
  return fetch(new URL(url, BASE));
}

export async function probeResWhole(url: string) {
  const res = await fetch(new URL(url, BASE));
  if (!res.ok) throw new Error('bad');
  return res;
}

export async function probeNested(url: string) {
  const res = await fetch(new URL(url, BASE));
  const json = (await res.json()) as Reply;
  return { meta: { res }, json };
}


export async function probeChain(url: string) {
  return callPlain(url);
}

export async function probeNoTransport(url: string) {
  const res = await fetch(new URL(url, BASE));
  const json = (await res.json()) as Reply;
  return { status: res.status, json };
}

export async function probeBody(url: string) {
  const res = await fetch(new URL(url, BASE));
  const json = (await res.json()) as Reply;
  return json.data;
}

export async function probeTuple(url: string) {
  const res = await fetch(new URL(url, BASE));
  return [res, (await res.json()) as Reply] as const;
}
