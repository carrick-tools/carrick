import { sharedQuery } from "./query";

const BASE = process.env.THINGS_API_URL;

type Options = { url: string; method?: string; body?: unknown };

async function send({ url, body }: Options) {
  const res = await fetch(url, { method: "POST", body: JSON.stringify(body ?? {}) });
  return res.json();
}

// The wrapper sends something other than what the caller wrote.
async function sendRewritten({ url }: Options) {
  url = url.replace("http:", "https:");
  const res = await fetch(url, { method: "POST" });
  return res.json();
}

// The wrapper supplies a verb of its own when the caller writes none.
async function sendDefaultVerb({ url, method }: Options) {
  const res = await fetch(url, { method: method ?? "POST" });
  return res.json();
}

// One return is not a query.
function looseQuery(filter: string | undefined): string {
  if (!filter) {
    return "";
  }
  if (filter.startsWith("/")) {
    return filter;
  }
  return `?filter=${filter}`;
}

// The value arrives through a promise.
async function asyncQuery(cursor?: string): Promise<string> {
  return cursor ? `?cursor=${cursor}` : "";
}

// One return is another call.
function recursiveQuery(parts: string[]): string {
  if (parts.length === 0) {
    return "";
  }
  if (parts.length > 8) {
    return recursiveQuery(parts.slice(0, 8));
  }
  return "?" + parts.join("&");
}

// One path returns nothing.
function partialQuery(cursor?: string) {
  if (cursor) {
    return `?cursor=${cursor}`;
  }
}

// A spread after the key may overwrite it.
export async function spreadAfter(extra: Partial<Options>) {
  return await send({ url: `${BASE}/refused/spread`, ...extra });
}

// A computed key after the key may overwrite it.
export async function computedAfter(name: string, value: string) {
  return await send({ url: `${BASE}/refused/computed`, [name]: value });
}

// The caller does not write the key.
export async function missingKey() {
  return await send({ body: { path: "/refused/missing" } } as Options);
}

// The object is written through after it is built.
export async function writtenThrough(other: string) {
  const request = { url: `${BASE}/refused/written` };
  request.url = other;
  return await send(request);
}

export async function rewritten() {
  return await sendRewritten({ url: `${BASE}/refused/rewritten` });
}

export async function defaultVerb() {
  return await sendDefaultVerb({ url: `${BASE}/refused/default-verb` });
}

export async function looseTail(id: string, filter?: string) {
  return await send({ url: `${BASE}/refused/loose/${id}${looseQuery(filter)}` });
}

// The builder is another module's.
export async function importedTail(id: string, cursor?: string) {
  return await send({ url: `${BASE}/refused/imported/${id}${sharedQuery(cursor)}` });
}

export async function asyncTail(id: string, cursor?: string) {
  return await send({ url: `${BASE}/refused/async/${id}${await asyncQuery(cursor)}` });
}

export async function recursiveTail(id: string, parts: string[]) {
  return await send({ url: `${BASE}/refused/recursive/${id}${recursiveQuery(parts)}` });
}

export async function partialTail(id: string, cursor?: string) {
  return await send({ url: `${BASE}/refused/partial/${id}${partialQuery(cursor)}` });
}

// A value glued after a segment that no function builds.
export async function plainTail(id: string, suffix: string) {
  return await send({ url: `${BASE}/refused/plain/${id}${suffix}` });
}

// A query that is not the end of the URL.
export async function queryThenPath(id: string, cursor?: string) {
  return await send({ url: `${BASE}/refused/middle/${id}${sharedLocalQuery(cursor)}/tail` });
}

function sharedLocalQuery(cursor?: string): string {
  return cursor ? `?cursor=${cursor}` : "";
}
