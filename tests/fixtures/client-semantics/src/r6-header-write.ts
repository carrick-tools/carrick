import http from "@fixture/http";

const api = http.create({ baseURL: "/r6" });
(api as any).defaults.headers.common["Authorization"] = "token";

export function r6(): unknown { return api.get("/header-write"); }
