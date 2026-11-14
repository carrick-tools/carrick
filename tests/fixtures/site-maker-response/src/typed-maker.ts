import { http } from "@fixture/http";

function makeReader<T>(root: string) {
  return () => http.remove<T>(`${root}/read`, { headers: {} });
}
export const readStock = makeReader<{ count: number }>("/stock");
