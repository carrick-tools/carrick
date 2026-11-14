import { send } from "@fixture/legacy";

async function ping(base: string): Promise<number> {
  const out = await send<{ n: number }>(`${base}/api/legacy`, { headers: {} });
  return out.n;
}
export const pingedLegacy = () => ping("/old");
