// An ES module written in TypeScript: the annotations parse only when the
// file is read as TypeScript.
import { roundCents } from "./rounding.cjs";

interface LineItem {
  unitPrice: number;
  quantity: number;
}

export function quoteOrder(items: LineItem[]): number {
  const raw = items.reduce((sum: number, item: LineItem) => sum + item.unitPrice * item.quantity, 0);
  return roundCents(raw);
}
