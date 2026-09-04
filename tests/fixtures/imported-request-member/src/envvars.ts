// The consumer of that exported instance. It names no path and no verb, and
// its site is the one the join counts and does not follow.
import { client } from "./instance.js";

export async function readVariables(name: string) {
  return client.readArtifactUrl(name);
}
