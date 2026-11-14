import { http } from "@fixture/http";

function runJob<T>(base: string): Promise<T> {
  const run = () => http.remove<T>(`${base}/run`, { headers: {} });
  return run();
}
export const job = () => runJob<{ done: boolean }>("/jobs");
