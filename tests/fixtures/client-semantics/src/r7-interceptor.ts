import http from "@fixture/http";

const api = http.create({ baseURL: "/r7" });
(api as any).interceptors.request.use((config: unknown) => config);

export function r7(): unknown { return api.get("/interceptor"); }
