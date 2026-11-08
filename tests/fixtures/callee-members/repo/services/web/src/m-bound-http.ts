import { http } from "@fixture/http";

function send(path: string) {
  return http.post(path, null);
}

export function f() {
  const g = send.bind(null, "/p/m-bound-http");
  return g();
}
