// The consumer two hops from the client: it imports only the factory, and the
// factory's module is what imports the client. Nothing here names the client
// module, states a path, or states a verb.
import { getProjectClient } from "./session.js";

export async function listVariables(ref: string, name: string) {
  const projectClient = await getProjectClient(ref);
  return projectClient.client.readArtifactUrl(name);
}
