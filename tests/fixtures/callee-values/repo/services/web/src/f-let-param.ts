import { send } from "./send";

let s = send;

export function set(next: typeof send) {
  s = next;
}

export function f() {
  return s("/p/f-let-param");
}
