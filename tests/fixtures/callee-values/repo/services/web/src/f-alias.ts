import { send } from "./send";

const s = send;

export function f() {
  return s("/p/f-alias");
}
