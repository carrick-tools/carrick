import { send, sendToo } from "./send";

function g(next: typeof send, flag: boolean) {
  let s = next;
  if (flag) {
    s = send;
  }
  return s("/p/f-local-init-param");
}

export function f() {
  return g(sendToo, true);
}
