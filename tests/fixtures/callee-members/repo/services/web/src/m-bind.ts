import { http } from "@fixture/http";

const post = http.post.bind(http);

export function f() {
  return post("/p/m-bind", null);
}
