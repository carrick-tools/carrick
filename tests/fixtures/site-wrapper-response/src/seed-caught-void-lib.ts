import { http } from "@fixture/http";
export function tryRemove(path: string) {
  return http.post<{ gone: boolean }>(path).catch(() => {});
}
