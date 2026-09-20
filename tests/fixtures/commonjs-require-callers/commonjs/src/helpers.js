function computeTotal(items) {
  return items.reduce((sum, item) => sum + item.price, 0);
}

function applyDiscount(total, rate) {
  return total - total * rate;
}

module.exports = { computeTotal, applyDiscount };
