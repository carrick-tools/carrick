import http from "@fixture/http";

const api = http.create({ baseURL: "/r5" });
(api as any).setBaseURL("/elsewhere");

export function r5(): unknown { return api.get("/setter"); }
