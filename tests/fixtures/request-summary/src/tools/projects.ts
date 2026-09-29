import type { ApiClient } from "../api-client.js";

export async function projects(client: ApiClient) {
  return client.listProjects();
}
