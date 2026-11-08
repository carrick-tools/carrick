import { http } from "@fixture/http";

function send(path: string) {
  return http.post(path, null);
}

const g = send.bind(null);

export function f() {
  return g("/p/m-own-bind");
}
