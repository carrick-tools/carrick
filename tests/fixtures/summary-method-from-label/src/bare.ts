import { http } from "@fixture/http";

async function clearAll(base: string): Promise<number> {
  const listing = await http.remove<{ items: string[] }>(`${base}/api/items`);
  return listing.items.length;
}
export const clearedAll = () => clearAll("/bare");
