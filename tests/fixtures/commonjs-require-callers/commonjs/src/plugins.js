const chosen = process.env.ORDER_PLUGIN;
const plugin = require(chosen);

function runPlugin(order) {
  return plugin.computeTotal(order.items);
}

module.exports = { runPlugin };
