import { refunds } from "./lib/refunds.js";

export function refundOrder(orderId: string, amount: number) {
  return refunds.issue({ orderId, amount });
}
