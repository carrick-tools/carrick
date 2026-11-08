import { http } from "@fixture/http";

export function f() {
  return http.post.bind(http)("/p/m-bind-inline", null);
}
