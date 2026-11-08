import { http } from "@fixture/http";

function send(base: string, rest: string) {
  return http.post(base + rest, null);
}

export function f() {
  const g = send.bind(null, "/p/m-bound-");
  return g("split");
}
