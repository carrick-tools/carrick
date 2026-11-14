import { http } from "@fixture/http";

async function rotate(base: string): Promise<void> {
  await http.remove(`${base}/slot`, { headers: {} });
  await http.post(`${base}/slot`, { headers: {} });
}
export const rotated = () => rotate("/yard");
