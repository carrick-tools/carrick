import type { ApiClient } from "./api-client.js";

export async function describeService(client: ApiClient, name: string) {
  const match = await client.findService(name);
  return match ?? null;
}
