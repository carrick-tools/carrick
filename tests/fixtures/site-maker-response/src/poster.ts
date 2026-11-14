import { http } from "@fixture/http";

export interface Order { sku: string; qty: number }
export function makeCanceller(prefix: string) {
  return (order: Order) => http.remove<{ id: string }>(`${prefix}/orders`, { data: order });
}
export const cancelOrder = makeCanceller("/shop");
