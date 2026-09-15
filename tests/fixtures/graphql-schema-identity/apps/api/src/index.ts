import { Hono } from 'hono';
import { createYoga } from 'graphql-yoga';
import { schema } from './schema';

const app = new Hono();
const yoga = createYoga({ schema });

app.get('/health', (c) => c.json({ ok: true }));

app.all('/graphql', (c) => yoga.fetch(c.req.raw));

export default app;
