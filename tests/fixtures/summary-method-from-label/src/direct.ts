import { http } from "@fixture/http";

export async function wipe(): Promise<{ ok: boolean }> {
  return http.remove<{ ok: boolean }>("/direct/items", { headers: {} });
}
