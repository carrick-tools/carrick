const { formatMoney: money } = require("./formatting");
const tax = require("./tax");

export function renderReceipt(order: { total: number }): string {
  return money(tax.applyTax(order.total));
}
