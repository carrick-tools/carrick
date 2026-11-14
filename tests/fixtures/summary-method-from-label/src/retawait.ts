import { http } from "@fixture/http";

async function dropAll(base: string): Promise<{ items: string[] }> {
  return await http.remove<{ items: string[] }>(`${base}/api/all`, { headers: {} });
}
export const droppedAll = () => dropAll("/again");
