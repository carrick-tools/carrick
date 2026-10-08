// A CommonJS module: script-shaped, `require` in and `module.exports` out.
const { format } = require("node:util");

function priceOrder(items) {
  return items.reduce((sum, item) => sum + item.unitPrice * item.quantity, 0);
}

function describePrice(amount) {
  return format("%d cents", amount);
}

module.exports = { priceOrder, describePrice };
