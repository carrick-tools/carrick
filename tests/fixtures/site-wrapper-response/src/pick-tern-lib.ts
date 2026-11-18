import { http } from "@fixture/http";
function a(p: string) {
  return http.post<{ a: number }>(`${p}/a`);
}
function b(p: string) {
  return http.post<{ b: number }>(`${p}/b`);
}
export function pick(p: string, c: boolean) {
  return c ? a(p) : b(p);
}
