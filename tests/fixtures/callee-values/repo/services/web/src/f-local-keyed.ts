import { send } from "./send";

export function f() {
  const d = { a: send };
  return d["a"]("/p/f-local-keyed");
}
