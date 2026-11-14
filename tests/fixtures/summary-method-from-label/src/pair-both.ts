import { http } from "@fixture/http";

async function clearBoth(base: string): Promise<void> {
  await Promise.all([http.remove(`${base}/left`, { headers: {} }), http.purge(`${base}/right`, { headers: {} })]);
}
export const clearedBoth = () => clearBoth("/twin");
