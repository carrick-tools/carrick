import { Hono } from 'npm:hono@4';
export const app = new Hono();
app.get('/healthz', (c) => c.json({ ok: true }));
