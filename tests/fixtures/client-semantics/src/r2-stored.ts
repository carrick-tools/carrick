import http from "@fixture/http";

const api = http.create({ baseURL: "/r2" });
const clients = [api];
const registry = { api };

export function r2a(): unknown { return clients[0].get("/array"); }
export function r2b(): unknown { return registry.api.get("/object"); }
export function r2c(): unknown { return api.get("/direct"); }
