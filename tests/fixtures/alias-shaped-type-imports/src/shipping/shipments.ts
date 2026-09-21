import { Hono } from 'hono';
import type { Shipment } from '@shipping/shipment.ts';

export const shipments = new Hono();

shipments.get('/shipments/:id', (c) => {
  const shipment: Shipment = { id: c.req.param('id'), carrier: 'air' };
  return c.json(shipment);
});
