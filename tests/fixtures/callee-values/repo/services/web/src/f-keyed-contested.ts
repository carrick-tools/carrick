import { send } from "./send";

export const d: { a: typeof send } = { a: send };

export function f() {
  return d["a"]("/p/f-keyed-contested");
}
