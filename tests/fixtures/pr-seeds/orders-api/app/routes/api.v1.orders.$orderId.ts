// GET /api/v1/orders/:orderId, from the file name and the exported loader.
import type { OrderRef } from "../../src/contracts";
export type Order = OrderRef & {
  id: string;
  total: number;
  currency: string;
};

export async function loader({
  params,
}: {
  params: { orderId: string };
}): Promise<Order> {
  return { id: String(params.orderId), total: 1200, currency: "EUR" };
}
