import { send } from "./send";

export function f() {
  let s: typeof send;
  s = send;
  return s("/p/f-local-assigned");
}
