import http from "@fixture/http";

const api = http.create({ baseURL: "/r3" });

export function getApi(): unknown { return api; }
export function r3(): unknown { return api.get("/returned"); }
