import http from "@fixture/http";

declare const prod: boolean;
const overrides: { baseURL?: string } = {};
if (prod) {
  overrides.baseURL = "/prod";
}
const api = http.create({ baseURL: "/dev", ...overrides });

export function n7(): unknown { return api.get("/mutated-const"); }
