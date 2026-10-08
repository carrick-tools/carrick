// A test file: the scan leaves it out, whatever the module extension.
import { priceOrder } from "./pricing.cjs";

export function checkPriceOrder() {
  return priceOrder([]) === 0;
}
