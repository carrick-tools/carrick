import { http } from "@fixture/http";
export function listOrDefault(path: string) {
  return http.post<{ rows: string[] }>(path).catch(() => ({ rows: [] as string[] }));
}
