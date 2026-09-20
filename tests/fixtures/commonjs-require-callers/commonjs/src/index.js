const { computeTotal } = require("./helpers");

function handleOrder(order) {
  return computeTotal(order.items);
}

module.exports = { handleOrder };
