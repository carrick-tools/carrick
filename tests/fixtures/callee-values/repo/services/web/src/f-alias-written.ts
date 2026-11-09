import { other, send } from "./send";

const d: { a: typeof send } = { a: send };
const g = d.a;

export function swap() {
  d.a = other as typeof send;
}

export function f() {
  return g("/p/f-alias-written");
}
