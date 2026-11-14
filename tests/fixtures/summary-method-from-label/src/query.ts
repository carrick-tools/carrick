import { http } from "@fixture/http";

async function clearWhere(query: string): Promise<number> {
  const listing = await http.remove<{ items: string[] }>(`/api/rows?${query}`, { headers: {} });
  return listing.items.length;
}
export const clearedWhere = () => clearWhere("stale=1");
