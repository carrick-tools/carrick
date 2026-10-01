import { connect } from "@fixture/broker";

const client = connect();

export function placeOrder(order: { id: string }): void {
  client.publish("orders.created", order);
}

export function watchCancellations(): void {
  client.subscribe("orders.cancelled");
}

export async function watchShipments(): Promise<void> {
  await client
    .subscribe("orders.shipped");
}
