import { http } from "@fixture/http";

async function removeOne(base: string, id: string): Promise<boolean> {
  const result = await http.remove<{ ok: boolean }>(`${base}/api/items`, { data: { id } });
  return result.ok;
}
export const removed = (id: string) => removeOne("/data", id);
