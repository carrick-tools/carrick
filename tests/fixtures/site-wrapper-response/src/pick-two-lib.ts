import { http } from "@fixture/http";
function a(p: string) {
  return http.post<{ a: number }>(`${p}/ta`);
}
function b(p: string) {
  return http.post<{ b: number }>(`${p}/tb`);
}
export function pick(p: string, c: boolean) {
  if (c) {
    return a(p);
  }
  return b(p);
}
