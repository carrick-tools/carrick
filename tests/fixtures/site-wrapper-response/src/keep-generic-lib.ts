import { http } from "@fixture/http";
export function drop<T>(base: string): Promise<T> {
  return http.remove<T>(`${base}/report`, { headers: {} });
}
