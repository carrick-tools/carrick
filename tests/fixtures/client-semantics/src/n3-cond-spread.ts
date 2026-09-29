import http from "@fixture/http";

declare const prod: boolean;
const api = http.create({ baseURL: "/dev", ...(prod ? { baseURL: "/prod" } : {}) });
const same = http.create({ baseURL: "/x", ...(prod ? { timeout: 1 } : { timeout: 2 }) });
const agree = http.create({ ...(prod ? { baseURL: "/same" } : { baseURL: "/same" }) });
const andSpread = http.create({ baseURL: "/and", ...(prod && { baseURL: "/and-prod" }) });

export function n3a(): unknown { return api.get("/cond"); }
export function n3b(): unknown { return same.get("/keep"); }
export function n3c(): unknown { return agree.get("/agree"); }
export function n3d(): unknown { return andSpread.get("/and"); }
