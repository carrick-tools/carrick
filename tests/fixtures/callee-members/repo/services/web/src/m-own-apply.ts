import { http } from "@fixture/http";

function send(path: string) {
  return http.post(path, null);
}

export function f() {
  return send.apply(null, ["/p/m-own-apply"]);
}
