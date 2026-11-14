import { http } from "@fixture/http";

export function makeGate({ base }: { base: string }) {
  return async (tag: string) =>
    http.remove<{ ok: boolean }>(`${base}/api/reset`, { headers: { "x-tag": tag } });
}
export const gate = makeGate({ base: "/svc" });
