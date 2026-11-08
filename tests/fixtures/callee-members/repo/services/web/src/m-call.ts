import { http } from "@fixture/http";

export function f() {
  return http.post.call(http, "/p/m-call", null);
}
