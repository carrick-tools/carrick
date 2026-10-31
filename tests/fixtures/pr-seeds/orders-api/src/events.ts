import { bus } from "@fixture/queue";

export type PaymentSettled = {
  orderId: string;
  amount: number;
};

export function listen(): void {
  bus.subscribe("payments.settled", (event: PaymentSettled) => {
    void event.amount;
  });
}

export async function announceShipped(orderId: string): Promise<void> {
  await bus.publish("orders.shipped", { orderId, carrier: "post" });
}
