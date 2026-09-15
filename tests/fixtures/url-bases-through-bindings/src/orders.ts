import config from "./config.ts";

export async function loadOrder(orderId: string) {
  const res = await fetch(`${config.ORDERS_API_URL}/v1/orders/${orderId}`, {
    method: "GET",
  });
  return res.json();
}
