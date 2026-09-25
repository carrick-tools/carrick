import client from "./client";

const inflight = new Map<string, Promise<Response>>();

export function loadItem(id: string): Promise<Response> {
  const pending = fetch(`/items/${id}`).finally(() => inflight.delete(id));
  inflight.set(id, pending);
  return pending;
}

export function saveItem(id: string, init: RequestInit): Promise<string | null> {
  return fetch(`/items/${id}`, init).then((res) => res.headers.get("etag"));
}

declare function callApi(method: string, path: string, body?: unknown): Promise<unknown>;

export async function renameItem(id: string, form: FormData): Promise<unknown> {
  return await callApi("PATCH", `/items/${id}/name`, { name: form.get("name") });
}

declare function itemUrl(id: string | null): string;

export async function removeItem(url: URL, init: RequestInit): Promise<Response> {
  return await fetch(itemUrl(url.searchParams.get("id")), init);
}

export async function refreshItem(id: string, init: RequestInit): Promise<Response> {
  return inflight.get(id) ?? fetch(`/items/${id}/refresh`, init);
}

export async function reloadThenDrop(id: string): Promise<void> {
  await fetch(`/items/${id}/draft`).then(() => client.delete(`/items/${id}/draft`));
}

declare const api: { request(config: unknown): Promise<unknown>; fetch(url: string): Promise<unknown> };
declare function send(path: string, options: { onDone: () => unknown }): Promise<unknown>;

export async function publishDraft(config: unknown): Promise<void> {
  await Promise.all([api.request(config), client.delete("/draft")]);
}

export async function fetchThenDrop(draftUrl: string, id: string): Promise<void> {
  await api.fetch(draftUrl).then(() => client.delete(`/items/${id}/draft`));
}

export async function sendThenDrop(id: string): Promise<void> {
  await send(`/items/${id}`, { onDone: () => client.delete(`/items/${id}`) });
}
