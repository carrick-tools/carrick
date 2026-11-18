import { http } from "@fixture/http";
export function runJob<T>(base: string): Promise<T> {
  const run = () => http.remove<T>(`${base}/run`, { headers: {} });
  return run();
}
