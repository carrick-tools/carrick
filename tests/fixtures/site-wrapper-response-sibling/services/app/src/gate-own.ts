import { http } from "@fixture/http";
export function makeOwnGate({ base }: { base: string }) {
  return async (tag: string) =>
    http.remove<{ ok: boolean }>(`${base}/api/own-reset`, { headers: { "x-tag": tag } });
}
