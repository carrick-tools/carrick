import type { ApiClient } from "./api-client.js";

export async function serviceGraph(client: ApiClient) {
  const repos = await client.getAllRepoData();
  return repos.length;
}
