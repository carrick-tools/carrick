import { createContext, createHooks } from "fixture-procs";
import type { AppRouter } from "../router";

const hooks = createHooks<AppRouter>();

export function useShelf() {
  const shelf = hooks.shelf.list.useQuery();
  const add = hooks.shelf.add.useMutation();
  return { shelf, add };
}

const kit = createContext<AppRouter>();

export function useCountOptions() {
  const proxy = kit.useProxy();
  return proxy.count.queryOptions();
}
