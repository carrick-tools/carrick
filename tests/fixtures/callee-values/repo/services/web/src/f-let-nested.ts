import { send, sendToo } from "./send";

let s = send;

export function later() {
  setTimeout(() => {
    s = sendToo;
  }, 0);
}

export function f() {
  return s("/p/f-let-nested");
}
