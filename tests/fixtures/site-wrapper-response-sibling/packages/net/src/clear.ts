import { http } from "@fixture/http";
export function clearAs<T>(path: string) {
  return http.purge<T>(path);
}
export function clearWhole(path: string) {
  return http.purge<{ wiped: number }>(path);
}
