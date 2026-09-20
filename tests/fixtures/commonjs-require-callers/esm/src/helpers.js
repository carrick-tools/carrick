export function computeTotal(items) {
  return items.reduce((sum, item) => sum + item.price, 0);
}

export function applyDiscount(total, rate) {
  return total - total * rate;
}
