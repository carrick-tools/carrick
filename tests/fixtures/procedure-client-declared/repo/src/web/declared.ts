import { createProxy, createHandle, createExpandedHooks, type ProxyOf } from "fixture-procs";
import type { AppRouter } from "../router";

// A client held in a field typed by an in-repo alias of the package's alias.
type RunnerClient = ProxyOf<AppRouter["record"]>;

export class Runner {
  client: RunnerClient = createProxy<AppRouter>({ url: "/api/rpc" });

  list() {
    return this.client.shelf.list.query();
  }

  add() {
    return this.client.shelf.add.mutate({ title: "x" });
  }
}

// An alias the value keeps its name under.
const handle = createHandle<AppRouter>({ url: "/api/rpc" });

export function listViaHandle() {
  return handle.shelf.list.query();
}

// An alias of an intersection, held in a const.
const hooks = createExpandedHooks<AppRouter>();

export function useShelf() {
  return hooks.shelf.list.useQuery();
}
