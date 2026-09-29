import http from "@fixture/http";

const api = http.create({ baseURL: "/v1" });

export function a2(): unknown {
  return api.get("/users", { baseURL: "/override" } as any);
}

export function a2b(): unknown {
  return http.get("/plain", { baseURL: "/elsewhere" } as any);
}
