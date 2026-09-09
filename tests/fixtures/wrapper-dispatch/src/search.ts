import type { ApiClient } from "./api-client.js";

export async function search(client: ApiClient, query: string) {
  const hits = await client.searchByIntent(query);
  return hits;
}
