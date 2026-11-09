import { send } from "./send";

const d = { a: send };
const g = d.a;

export function f() {
  return g("/p/f-alias-dot");
}
