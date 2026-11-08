import { other, send } from "./send";

export function f(flag: boolean) {
  let s = send;
  if (flag) {
    s = other as typeof send;
  }
  return s("/p/f-local-let");
}
