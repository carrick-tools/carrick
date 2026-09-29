import http from "@fixture/http";

const api = http.create({ baseURL: "/r4" });
export const version = (http as any).VERSION;

export function r4(): unknown { return api.get("/const-read"); }
