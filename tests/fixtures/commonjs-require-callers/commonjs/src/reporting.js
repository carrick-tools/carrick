const helpers = require("./helpers");
const computeTotal = require("./helpers").computeTotal;

function summarise(order) {
  const total = computeTotal(order.items);
  return helpers.applyDiscount(total, order.rate);
}

module.exports = { summarise };
