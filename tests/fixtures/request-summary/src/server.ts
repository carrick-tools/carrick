import type { ApiClient } from "./api-client.js";

type ReadState = (deps: {
  jobStatus: (repo: string) => Promise<unknown>;
}) => Promise<unknown>;

export async function handle(client: ApiClient, readState: ReadState) {
  const state = await readState({
    jobStatus: (repo) => client.analysisJobStatus(repo),
  });
  if (!state) {
    client.invalidateCache();
    return client.getAllRepoData();
  }
  return state;
}

export function boot(client: ApiClient) {
  client.startPolling();
}
