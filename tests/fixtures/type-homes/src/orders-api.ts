import { Hono } from 'hono';
import type { CreateOrder, Order } from './models';

export const orders = new Hono();

orders.get('/orders/:id', (c) => {
  const order: Order = { id: c.req.param('id'), sku: 'a', quantity: 1 };
  return c.json(order);
});

orders.post('/orders', async (c) => {
  const draft: CreateOrder = await c.req.json();
  return c.json({ accepted: draft.sku });
});
