import type { Store } from "./ports";

export function makeStore(): Store {
  const read = async (key: string) => key.length;
  return { read };
}
