const { computeTotal } = require("./barrel");

function auditOrder(order) {
  return computeTotal(order.items) > 0;
}

module.exports = { auditOrder };
