import { http } from "@fixture/http";

async function vacate(base: string): Promise<void> {
  await Promise.all([http.remove(`${base}/slot`, { headers: {} })]);
}
export const vacated = () => vacate("/dock");
