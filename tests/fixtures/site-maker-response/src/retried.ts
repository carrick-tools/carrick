import { http } from "@fixture/http";

function retry<R>(attempt: () => R): R { return attempt(); }
function retried<T>(base: string): Promise<T> {
  const run = () => http.remove<T>(`${base}/retry`, { headers: {} });
  return retry(run);
}
export const again = () => retried<{ n: number }>("/tasks");
