import http from "@fixture/http";

declare const defaults: { timeout?: number };
const api = http.create({ ...defaults, baseURL: "/after" });

export function n1(): unknown {
  return api.get("/stated");
}
