import { send, sendToo } from "./send";

export function f() {
  let s = send;
  setTimeout(() => {
    s = sendToo;
  }, 0);
  return s("/p/f-local-nested");
}
