import { http } from "@fixture/http";
export function inner(path: string) {
  return http.post<{ depth: number }>(path);
}
export function outer(path: string) {
  return inner(path);
}
