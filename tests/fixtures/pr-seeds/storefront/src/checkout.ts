import type { OrdersClient } from "./orders-client.js";

export async function totalOf(orderId: string, client: OrdersClient): Promise<number> {
  const order = await client.readOrder(orderId);
  return order.total;
}
