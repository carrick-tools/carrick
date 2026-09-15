import { settings } from "./settings";

export class RequestFailed extends Error {
  constructor(readonly status: number) {
    super(`request failed with ${status}`);
  }
}

export async function sendJson<T>(verb: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${settings.gatewayUrl}${path}`, {
    method: verb,
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) {
    throw new RequestFailed(res.status);
  }
  return (await res.json()) as T;
}
