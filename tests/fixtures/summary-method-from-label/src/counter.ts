import { http } from "@fixture/http";

async function clearItems(base: string): Promise<number> {
  const listing = await http.remove<{ items: string[] }>(`${base}/api/items`, { headers: {} });
  return listing.items.length;
}
export const cleared = () => clearItems("/stock");
