import { createStore } from "@acme/store";

const store = createStore();

export function lookup(key: string) {
  return store.cache.get(key);
}
