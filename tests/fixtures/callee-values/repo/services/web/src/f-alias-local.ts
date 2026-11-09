import { send } from "./send";

const d = { a: send };

export function f() {
  const g = d.a;
  return g("/p/f-alias-local");
}
