import { other, send } from "./send";

let s = send;

export function swap() {
  s = other as typeof send;
}

export function f() {
  return s("/p/f-let");
}
