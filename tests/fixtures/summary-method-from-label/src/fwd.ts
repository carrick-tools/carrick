import { http } from "@fixture/http";

async function dropOne(base: string, id: string): Promise<boolean> {
  const result = await http.remove<{ ok: boolean }>(`${base}/items/${id}`, { headers: {} });
  return result.ok;
}
export const droppedOne = (id: string) => dropOne("/fwd", id);
