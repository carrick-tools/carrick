const BASE = process.env.THINGS_API_URL;

type Options = { url: string; method?: string; body?: unknown };

// A module function that destructures its options in the parameter list.
async function send({ url, body }: { url: string; body?: unknown }) {
  const res = await fetch(url, { method: "PUT", body: JSON.stringify(body ?? {}) });
  return res.json();
}

// The options read by member.
async function sendByMember(options: Options) {
  const res = await fetch(options.url, { method: "PATCH", body: JSON.stringify(options.body) });
  return res.json();
}

// The options unpacked in the body.
async function sendUnpacked(options: Options) {
  const { url, body } = options;
  const res = await fetch(url, { method: "DELETE", body: JSON.stringify(body) });
  return res.json();
}

// The key bound under another name.
async function sendRenamed({ url: target }: Options) {
  const res = await fetch(target, { method: "POST" });
  return res.json();
}

// The caller writes the verb as well as the URL.
async function sendWithVerb({ url, method }: Options) {
  const res = await fetch(url, { method });
  return res.json();
}

// A query builder declared at module scope.
function pageQuery(cursor: string | undefined): string {
  if (!cursor) {
    return "";
  }
  return `?cursor=${encodeURIComponent(cursor)}`;
}

export async function renameThing(id: string, name: string) {
  return await send({ url: `${BASE}/things/${id}/name`, body: { name } });
}

export async function touchThing(id: string) {
  return await sendByMember({ url: `${BASE}/things/${id}/touch` });
}

export async function dropThing(id: string) {
  return await sendUnpacked({ url: `${BASE}/things/${id}` });
}

export async function copyThing(id: string) {
  return await sendRenamed({ url: `${BASE}/things/${id}/copies` });
}

export async function listThings(cursor?: string) {
  return await sendWithVerb({ url: `${BASE}/things${pageQuery(cursor)}`, method: "GET" });
}

export async function countThings() {
  return await sendWithVerb({ url: `${BASE}/things/count` });
}
