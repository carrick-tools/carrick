import { formatMoney as money } from "./formatting.js";
import * as tax from "./tax.js";

export function renderReceipt(order: { total: number }): string {
  return money(tax.applyTax(order.total));
}
