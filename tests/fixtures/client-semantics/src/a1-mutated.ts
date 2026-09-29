import http from "@fixture/http";

const api = http.create({ baseURL: "/v1" });
(api as any).defaults.baseURL = "/v2";

export function a1(): unknown {
  return api.get("/mutated");
}
