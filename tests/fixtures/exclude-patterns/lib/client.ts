const ORDERS_URL = "/api/orders";

export async function loadOrders() {
  const response = await fetch(ORDERS_URL);
  return response.json();
}
