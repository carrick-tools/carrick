import { send } from "./send";

const d = { a: send };
const k = "a" as const;

export function f() {
  return d[k]("/p/f-keyed-const");
}
