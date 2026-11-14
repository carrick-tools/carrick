import { http } from "@fixture/http";

function dropJson<T>(base: string): Promise<T> {
  return http.remove<T>(`${base}/api/report`, { headers: {} });
}
export const dropped = () => dropJson<{ rows: number }>("/ledger");
