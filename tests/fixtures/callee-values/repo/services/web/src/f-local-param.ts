import { send } from "./send";

export function f(next: typeof send, flag: boolean) {
  let s = send;
  if (flag) {
    s = next;
  }
  return s("/p/f-local-param");
}
