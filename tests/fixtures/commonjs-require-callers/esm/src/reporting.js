import * as helpers from "./helpers.js";
import { computeTotal } from "./helpers.js";

export function summarise(order) {
  const total = computeTotal(order.items);
  return helpers.applyDiscount(total, order.rate);
}
