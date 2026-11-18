import { reply } from "@fixture/reply";
export interface Order { id: string }
export async function listOrders(p: string): Promise<Order[]> {
  const res = await reply.get<any>(`${p}/orders-if`);
  if (!res.data) return [];
  return res.data;
}
