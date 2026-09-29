import http from "@fixture/http";

const api = http.create({ baseURL: "/r4b" });

export function isHttpError(e: unknown): boolean { return e instanceof (http as any).HttpError; }
export function r4b(): unknown { return api.get("/instanceof"); }
