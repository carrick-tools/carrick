import { computeTotal } from "./barrel.js";

export function auditOrder(order) {
  return computeTotal(order.items) > 0;
}
