import { Hono } from 'hono';
import { createYoga, createSchema } from 'graphql-yoga';
import { readFileSync } from 'node:fs';

interface Invoice {
  id: string;
  total: number;
  dueAt: string | null; status: 'DRAFT' | 'SENT';
}

const invoices = new Map<string, Invoice>();

const resolvers = {
  Query: {
    invoice: (_parent: unknown, args: { id: string }): Invoice | null => invoices.get(args.id) ?? null,
  },
  Mutation: {
    sendInvoice: (_parent: unknown, args: { id: string }): Invoice => ({ id: args.id, total: 0, dueAt: null, status: 'DRAFT' }),
  },
};

const yoga = createYoga({
  schema: createSchema({
    typeDefs: readFileSync(new URL('./schema.graphql', import.meta.url), 'utf8'),
    resolvers,
  }),
});

const app = new Hono();

app.get('/health', (c) => c.json({ ok: true }));

app.all('/graphql', (c) => yoga.fetch(c.req.raw));

export default app;
