import { Hono } from 'hono';
import { guard, record, text } from '@fixture/checks';
import { NewTicket } from './schemas';

export const tickets = new Hono();

tickets.post('/tickets', guard('json', NewTicket), async (c) => {
  return c.json({ ok: true });
});

tickets.post('/drafts', async (c) => {
  const parsed = NewTicket.safeParse(await c.req.json());
  return c.json({ ok: parsed.success });
});

tickets.post('/notes', guard('json', record({ body: text() })), async (c) => {
  return c.json({ ok: true });
});
