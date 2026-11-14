import { http } from "@fixture/http";

export async function clearShelf(base: string): Promise<number> {
  const listing = await http.remove<{ items: string[] }>(`${base}/api/shelf`, { headers: {} });
  return listing.items.length;
}
