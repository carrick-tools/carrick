import { bus } from '@fixture/queue';

export async function announce(sku: string, quantity: number): Promise<void> {
  await bus.publish('orders.created', { topic: 'orders.created', sku, quantity });
  await bus.publish('orders.paid', { topic: 'orders.paid', sku });
}
