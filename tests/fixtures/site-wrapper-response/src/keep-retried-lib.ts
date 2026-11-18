import { http } from "@fixture/http";
export function retry<R>(attempt: () => R): R { return attempt(); }
export function retried<T>(base: string): Promise<T> {
  const run = () => http.remove<T>(`${base}/retry`, { headers: {} });
  return retry(run);
}
