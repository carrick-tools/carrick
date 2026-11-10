import { createClient } from "fixture-procs";
import type { RemoteRouter } from "fixture-remote";

const remote = createClient<RemoteRouter>({ url: "/files/rpc" });

export async function listFiles() {
  return remote.files.list.query();
}
