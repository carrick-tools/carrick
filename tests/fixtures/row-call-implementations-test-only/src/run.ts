import { makeStore } from "./store";
import type { Orphan, Store } from "./ports";

export function runEach(f: (key: string) => Promise<number>): Promise<number> {
  return f("a");
}

export function direct(store: Store): Promise<number> {
  return store.read("x");
}

export async function main(): Promise<number> {
  const store = makeStore();
  return runEach((key) => store.read(key));
}

export function pingIt(o: Orphan): void {
  o.ping();
}
