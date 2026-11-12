import { bus } from "../bus";

export async function publishCreated(orderId: string): Promise<void> {
  await bus.publish("orders.created", { orderId, at: Date.now() });
}
