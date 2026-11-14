import { http } from "@fixture/http";

function dropItems(base: string): Promise<{ items: string[] }> {
  return http.remove<{ items: string[] }>(`${base}/api/items`, { headers: {} });
}
export const dropped = () => dropItems("/back");
