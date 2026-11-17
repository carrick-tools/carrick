import { reply } from "@fixture/reply";
export interface Order { id: string }
export async function orderItems(path: string) {
  const { data } = await reply.get<{ items: Order[] }>(path);
  const { items } = data;
  return items;
}
