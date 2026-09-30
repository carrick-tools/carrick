import { orders } from "./lib/orders.js";

export function checkout(sku: string) {
  return orders.place({ sku });
}
