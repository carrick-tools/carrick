const { formatTotal } = require("./vendor/money") as typeof import("../common/money");
const { roundCents } = require("./vendor/money");
const money = require("./vendor/money") as typeof import("../common/money");

export function checkout(c: number) {
  const total = formatTotal(c);
  const raw = roundCents(c);
  const rounded = money.roundCents(c);
  return total + raw + rounded;
}

export function applyFee(fee: typeof formatTotal) {
  return fee(1);
}
