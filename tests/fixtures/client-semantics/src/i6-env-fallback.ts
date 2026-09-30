import { envApi } from "./shared/env-api";

const local = process.env.SHARED_API_URL ?? "http://localhost:4016";
export const l = local;
export function i6(): unknown { return envApi.get("/read"); }
