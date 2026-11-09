import { send } from "./send";

export function f(x: { a?: typeof send }) {
  const d = { a: send, ...x };
  const g = d.a;
  return g("/p/f-alias-spread");
}
