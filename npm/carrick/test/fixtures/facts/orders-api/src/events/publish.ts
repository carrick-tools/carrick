import { bus } from "../bus";

export async function publishOrderCreated(orderId: string): Promise<void> {
  await bus.publish("orders.created", { orderId, at: Date.now() });
}
