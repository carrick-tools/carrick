import { Hono } from 'hono';

const app = new Hono();

app.get('/session', (c) => c.json({ signedIn: true }));

export default app;
