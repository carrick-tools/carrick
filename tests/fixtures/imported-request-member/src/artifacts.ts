import type { ApiClient } from "./apiClient.js";
import { apiClient } from "./legacy.js";

export async function stashArtifact(name: string, client: ApiClient) {
  const handle = await client.createArtifactUrl(name);

  if (!handle) {
    // The only path-shaped literal in this file, and it names neither call.
    throw new Error("Handle request failed; the server must serve /api/v2/artifacts");
  }

  return handle;
}

export async function loadArtifact(name: string, client: ApiClient) {
  const handle = await client.readArtifactUrl(name);
  return handle;
}

export async function localHandle(name: string) {
  return apiClient.createArtifactUrl(name);
}
