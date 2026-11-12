import { bus } from "../bus";

export async function onOrderCreated(message: { orderId: string }): Promise<void> {
  console.log("created", message.orderId);
}

bus.subscribe("orders.created", onOrderCreated);
