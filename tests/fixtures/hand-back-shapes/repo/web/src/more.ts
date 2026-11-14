import type { Reply } from './types';

const BASE = 'http://localhost:4100';

function pack(res: Response, json: Reply) {
  return { res, json };
}

class Holder {
  constructor(public res: Response, public json: Reply) {}
}

async function send(url: string) {
  return fetch(new URL(url, BASE));
}

export interface Out {
  res: Response;
  json: Reply;
}

export async function viaPack(url: string) {
  const res = await fetch(new URL(url, BASE));
  return pack(res, (await res.json()) as Reply);
}

export async function viaNew(url: string) {
  const res = await fetch(new URL(url, BASE));
  return new Holder(res, (await res.json()) as Reply);
}

export async function viaSpread(url: string) {
  const res = await fetch(new URL(url, BASE));
  const extra = { at: Date.now() };
  return { ...extra, res, json: (await res.json()) as Reply };
}

export async function viaClone(url: string) {
  const res = await fetch(new URL(url, BASE));
  return { res: res.clone(), json: (await res.json()) as Reply };
}

export async function viaSend(url: string) {
  const res = await send(url);
  return { res, json: (await res.json()) as Reply };
}


export async function viaDeclared(url: string): Promise<Out> {
  const res = await fetch(new URL(url, BASE));
  return { res, json: (await res.json()) as Reply };
}

export async function viaTuple(url: string) {
  const res = await fetch(new URL(url, BASE));
  return [res, (await res.json()) as Reply] as const;
}

export async function viaHeaders(url: string) {
  const res = await fetch(new URL(url, BASE));
  return { headers: res.headers, json: (await res.json()) as Reply };
}
