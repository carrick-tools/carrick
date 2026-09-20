import helpers = require("./helpers");

export function legacyTotal(order: { items: { price: number }[] }): number {
  return helpers.computeTotal(order.items);
}
