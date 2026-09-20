import { computeTotal } from "./helpers.js";

export function handleOrder(order) {
  return computeTotal(order.items);
}
