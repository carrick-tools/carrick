import { send, sendToo } from "./send";

let s = send;

export function swap() {
  s = sendToo;
}

export function f() {
  return s("/p/f-let-agree");
}
