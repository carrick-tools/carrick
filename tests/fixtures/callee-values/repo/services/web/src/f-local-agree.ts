import { send, sendToo } from "./send";

export function f(flag: boolean) {
  let s = send;
  if (flag) {
    s = sendToo;
  }
  return s("/p/f-local-agree");
}
