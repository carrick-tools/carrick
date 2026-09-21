import { Hono } from 'hono';
import type { Invoice } from '@invoicing/contracts/invoice';

export const invoices = new Hono();

invoices.get('/invoices/:number', (c) => {
  const invoice: Invoice = { number: c.req.param('number'), cents: 0 };
  return c.json(invoice);
});
