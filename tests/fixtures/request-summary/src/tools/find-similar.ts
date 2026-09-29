import type { ApiClient } from "../api-client.js";

export async function findSimilarFunctions(client: ApiClient, names: string[]) {
  const hits = await client.findSimilar({ names });
  return hits.slice(0, 10);
}
