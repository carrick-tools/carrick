import { http } from "@fixture/http";

async function shuffle(base: string): Promise<void> {
  await Promise.all([http.post(`${base}/spot`, { headers: {} }), http.remove(`${base}/spot`, { headers: {} })]);
}
export const shuffled = () => shuffle("/lot");
