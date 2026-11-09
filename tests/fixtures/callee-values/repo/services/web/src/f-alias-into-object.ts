import { send } from "./send";

const o = { post: send };
const h = { post: o.post };

export function f() {
  return h.post("/p/f-alias-into-object");
}
