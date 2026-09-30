import http from "@fixture/http";

const SHARED_API = process.env.SHARED_API_URL ?? "https://shared.example";
export const envApi = http.create({ baseURL: SHARED_API });
export function own(): unknown { return envApi.get("/own"); }
