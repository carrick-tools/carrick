import { defineFn } from "hub-kit";

const shipped = defineFn("orders");

export function ship(): void {
  const payload = {};
  shipped.handler("orders.shipped", payload);
}
