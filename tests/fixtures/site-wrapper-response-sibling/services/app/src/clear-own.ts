import { http } from "@fixture/http";
export function clearOwnAs<T>(path: string) {
  return http.purge<T>(path);
}
export function clearOwnWhole(path: string) {
  return http.purge<{ wiped: number }>(path);
}
