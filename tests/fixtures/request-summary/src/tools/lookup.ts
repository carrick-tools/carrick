import type { ApiClient } from "../api-client.js";

// The receiver is not named like a client, so the candidate scanner raises
// nothing here: only the call graph says where this call goes.
export async function lookup(gateway: ApiClient, query: string) {
  return gateway.searchByIntent(query);
}
