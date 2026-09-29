import http from "@fixture/http";

declare const a: object;
declare const b: object;
const api = http.create({ ...a, baseURL: "/between", ...b });

export function n2(): unknown {
  return api.get("/between");
}
