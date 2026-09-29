import http from "@fixture/http";

const api = http.create({ baseURL: "/r4c" });

export function r4c(): unknown { return api.get("/control"); }
