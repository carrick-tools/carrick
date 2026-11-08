import { http } from "@fixture/http";

type Deps = { post?: (path: string, data?: unknown) => Promise<unknown> };

const deps: Deps = {};
const post = deps.post ?? http.post.bind(http);

export function f() {
  return post("/p/m-fallback", null);
}
