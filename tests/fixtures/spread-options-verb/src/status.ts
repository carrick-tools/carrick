export function readStatus(url: string): Promise<Response> {
  return fetch(url, { headers: { Accept: "application/json" } });
}
