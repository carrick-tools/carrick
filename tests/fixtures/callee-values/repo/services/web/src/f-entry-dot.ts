import { send } from "./send";

const d = { a: send };

export function f() {
  return d.a("/p/f-entry-dot");
}
