import http from "@fixture/http";

const api = http.create({ baseURL: "/v1" });
const defaults = (api as any).defaults;
defaults.baseURL = "/v2";

const api2 = http.create({ baseURL: "/w1" });
Object.assign((api2 as any).defaults, { baseURL: "/w2" });

export function n8a(): unknown { return api.get("/alias-write"); }
export function n8b(): unknown { return api2.get("/assign-write"); }
