// A factory that constructs the client and hands it back inside a record. The
// consumers of that record never import the client module themselves, so the
// client is two import hops from the site that calls it.
import { ApiClient } from "./apiClient.js";

export async function getProjectClient(ref: string) {
  const client = new ApiClient(`https://api.example.test/${ref}`);
  return { id: ref, client };
}
