import { http } from "@fixture/http";

const post = http.post;

export function f() {
  return post("/p/m-alias-member", null);
}
