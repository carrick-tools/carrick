import { send, sendToo } from "./send";

function g(next: typeof send, flag: boolean) {
  let s = send;
  if (flag) {
    s = next;
  }
  return s("/p/f-local-param-called");
}

export function f() {
  return g(sendToo, true);
}
