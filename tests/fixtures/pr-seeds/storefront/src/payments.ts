import { bus } from "@fixture/queue";

export type PaymentSettled = {
  orderId: string;
  amount: number;
};

export async function settle(orderId: string, amount: number): Promise<void> {
  const event: PaymentSettled = { orderId, amount };
  await bus.publish("payments.settled", event);
}

export function onShipped(): void {
  bus.subscribe("orders.shipped", (event: { orderId: string; carrier: string }) => {
    void event.carrier;
  });
}
