import { send } from "./send";

let s = send;

export function f() {
  return s("/p/f-let-never");
}
