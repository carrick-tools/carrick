import { http } from "@fixture/http";

async function swap(base: string): Promise<void> {
  await Promise.all([http.remove(`${base}/slot`, { headers: {} }), http.post(`${base}/slot`, { headers: {} })]);
}
export const swapped = () => swap("/bay");
