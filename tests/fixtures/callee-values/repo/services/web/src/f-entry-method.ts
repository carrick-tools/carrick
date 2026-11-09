import { send } from "./send";

const d = {
  a(p: string) {
    return send(p);
  },
};

export function f() {
  return d.a("/p/f-entry-method");
}
