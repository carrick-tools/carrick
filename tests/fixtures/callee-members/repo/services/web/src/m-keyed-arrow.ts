import { http } from "@fixture/http";

const api = { send: (p: string) => http.post(p, null) };

export function f() {
  return api["send"]("/p/m-keyed-arrow");
}
