import type { ApiClient } from "./api-client.js";

export type MakeClient = (projectId: string) => ApiClient;

export async function syncProject(make: MakeClient) {
  const client = make("p1");
  const repos = await client.getAllRepoData();
  await client.refreshEverything();
  await client.send("purge-cache", {});
  return repos.length;
}
