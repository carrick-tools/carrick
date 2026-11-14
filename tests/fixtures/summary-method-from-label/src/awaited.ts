import { http } from "@fixture/http";

export async function wipeNow(): Promise<boolean> {
  const result = await http.remove<{ ok: boolean }>("/now/items", { headers: {} });
  return result.ok;
}
