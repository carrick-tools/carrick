const ORDERS_URL = "/api/orders";

export async function backfillOrders() {
  const response = await fetch(ORDERS_URL, { method: "POST", body: "{}" });
  return response.ok;
}
