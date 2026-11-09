import { other, send } from "./send";

const d: { a: typeof send } = { a: send };

export function swap() {
  d.a = other as typeof send;
}

export function f() {
  return d["a"]("/p/f-keyed-written");
}
