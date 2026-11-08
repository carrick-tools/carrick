const { formatTotal } = require("./vendor/money") as typeof import("../common/money");
const { roundCents } = require("./vendor/money");
const money = require("./vendor/money") as typeof import("../common/money");

export function checkout(c: number) {
  return formatTotal(c) + money.roundCents(c) + roundCents(c);
}

export function applyFee(fee: typeof formatTotal) {
  return fee(1);
}
