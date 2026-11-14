import { http } from "@fixture/http";

export async function dropShelf(base: string, id: string): Promise<boolean> {
  const result = await http.remove<{ ok: boolean }>(`${base}/shelves/${id}`, { headers: {} });
  return result.ok;
}
