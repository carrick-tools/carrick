import { http } from "@fixture/http";
export function load(path: string) {
  return http.post<{ total: number }>(path);
}
