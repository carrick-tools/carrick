import { Hono } from 'hono';
import { createYoga, createSchema } from 'graphql-yoga';
import { readFileSync } from 'node:fs';

const yoga = createYoga({
  schema: createSchema({ typeDefs: readFileSync(new URL('./schema.graphql', import.meta.url), 'utf8') }),
});

const app = new Hono();

app.get('/orders/export', (c) => c.json({ rows: [] }));

app.all('/graphql', (c) => yoga.fetch(c.req.raw));

export default app;
