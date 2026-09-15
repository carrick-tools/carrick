import { Hono } from 'hono';
import { createYoga } from 'graphql-yoga';
import { schema } from './graphql/schema';

const app = new Hono();
const yoga = createYoga({ schema });

app.get('/health', (c) => c.json({ ok: true }));

app.post('/widgets/import', async (c) => {
  const body: { rows: string[] } = await c.req.json();
  return c.json({ imported: body.rows.length });
});

app.all('/graphql', (c) => yoga.fetch(c.req.raw));

export default app;
