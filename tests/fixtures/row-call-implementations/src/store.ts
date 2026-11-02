import { gql, rpc } from "./transports.js";
import type { Store } from "./ports.js";

// The implementation is an object literal: a closure, an arrow and a method.
export function makeStore(): Store {
  async function read(k: string) {
    return gql.request(k);
  }
  const write = (k: string, v: number) => {
    void rpc.call(k);
  };
  return {
    read,
    write,
    remove(k: string) {
      void k;
    },
  };
}
