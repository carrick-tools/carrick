import { Hono } from 'hono';
import type { Receipt } from '~receipts/receipt';

export const receipts = new Hono();

receipts.get('/receipts/:reference', (c) => {
  const receipt: Receipt = { reference: c.req.param('reference'), paid: true };
  return c.json(receipt);
});
