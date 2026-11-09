import { send } from "./send";

export const d = { a: send };

export function f() {
  return d["a"]("/p/f-keyed-exported-open");
}
