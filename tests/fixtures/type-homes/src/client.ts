import type { KitResponse } from 'http-kit';
import type { NewItem, Order, Page } from './models';

export async function createItem(item: NewItem) {
  await fetch('/api/items', { method: 'POST', body: JSON.stringify(item) });
}

export async function listOrders() {
  const res = await fetch('/api/orders');
  return (await res.json()) as Page<Order>;
}

export async function loadBadge() {
  const res: KitResponse<Order> = await fetch('/api/badge').then((r) => r.json());
  return res;
}
