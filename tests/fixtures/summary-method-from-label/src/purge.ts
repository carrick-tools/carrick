import { http } from "@fixture/http";

async function purgeCache(base: string): Promise<boolean> {
  const result = await http.purge<{ ok: boolean }>(`${base}/cache`, { headers: {} });
  return result.ok;
}
export const purged = () => purgeCache("/edge");
