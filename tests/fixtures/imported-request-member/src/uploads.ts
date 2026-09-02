// A second consumer of the same client, for the shape where extraction emits
// no row for either site at all. Nothing here states a path or a verb, and
// unlike artifacts.ts there is no path-shaped literal to misread either.
import type { ApiClient } from "./apiClient.js";

export async function pushArtifact(name: string, client: ApiClient) {
  return client.createArtifactUrl(name);
}

export async function pullArtifact(name: string, client: ApiClient) {
  return client.readArtifactUrl(name);
}
