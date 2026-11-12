import { createClient } from "fixture-procs";
import type { GhostRouter } from "../ghost-router";

const ghost = createClient<GhostRouter>({ url: "/api/rpc" });

export async function listGhosts() {
  return ghost.ghosts.list.query();
}

export async function openGhost(id: string) {
  return ghost.ghosts.reopen.mutate({ id });
}
