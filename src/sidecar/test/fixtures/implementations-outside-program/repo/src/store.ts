import type { Store } from "./ports";

export function makeStore(): Store {
  function read(key: string): number {
    return key.length;
  }
  return { read };
}
