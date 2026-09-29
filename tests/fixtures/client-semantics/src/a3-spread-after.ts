import http from "@fixture/http";

declare const overrides: { baseURL?: string };
const api = http.create({ baseURL: "/v1", ...overrides });

export function a3(): unknown {
  return api.get("/spread");
}

export function a3b(cfg: object): unknown {
  return http.request({ url: "/cfg", method: "post", ...cfg });
}
