import { http } from "@fixture/http";

async function ping(base: string): Promise<boolean> {
  const result = await http.post<{ up: boolean }>(`${base}/ping`, { headers: {} });
  return result.up;
}
export const pinged = () => ping("/health");
