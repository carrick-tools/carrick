import { Hono } from 'hono';
import { DeliveryService } from '@/deliveries/delivery.service.ts';

const app = new Hono();
const deliveries = new DeliveryService();

app.get('/lockers/:id/plan', (c) => {
  return c.json({ ok: deliveries.plan(c.req.param('id')) });
});

export default app;
