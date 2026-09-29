import type { ApiClient } from "../api-client.js";

export async function checkCompatibility(client: ApiClient, consumer: string) {
  const repos = await client.getAllRepoData();
  return repos.filter((repo) => (repo as { name: string }).name === consumer);
}
