import { reply } from "@fixture/reply";
export interface Order { id: string }
export async function listOrders(path: string) {
  const res = await reply.get<Order[]>(path);
  return res.data;
}
