import { send } from "./send";

export function f(x: { a?: typeof send }) {
  const d = { a: send, ...x };
  return d["a"]("/p/f-keyed-spread");
}
