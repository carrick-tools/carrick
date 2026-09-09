import type { ApiClient } from "./api-client.js";

export async function refresh(client: ApiClient) {
  await client.refreshEverything();
}
