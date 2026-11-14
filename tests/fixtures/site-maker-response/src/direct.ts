import { http } from "@fixture/http";

export const purgeAll = () => http.remove<{ ok: boolean }>("/direct/purge", { headers: {} });
