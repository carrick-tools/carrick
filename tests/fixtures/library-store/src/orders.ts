import { bus } from "@fixture/queue";

export async function placeOrder(id: string): Promise<void> {
  await bus.publish("orders.created", { id });
}
