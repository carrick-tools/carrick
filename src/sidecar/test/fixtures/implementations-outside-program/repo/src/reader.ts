import type { Store } from "./ports";
import { makeStore } from "./store";

export function readOne(store: Store): number {
  return store.read("k");
}

export function useIt(): number {
  return readOne(makeStore());
}
