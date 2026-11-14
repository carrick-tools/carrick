import { http } from "@fixture/http";
export function tryPost(path: string) {
  return http.post<{ c: number }>(path).catch(() => null);
}
