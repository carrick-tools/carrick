export function listOrders() {
  return fetch("/api/orders");
}

export function addItem() {
  return fetch("/api/items", { method: "POST" });
}
