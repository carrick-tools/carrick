import { Hono } from 'hono';
import { createYoga, createSchema } from 'graphql-yoga';
import { readFileSync } from 'node:fs';

interface Invoice {
  id: string;
  total: number;
}

interface Settings {
  prefix: string;
}

interface Customer {
  id: string;
  name: string;
}

const invoices = new Map<string, Invoice>();
const customers = new Map<string, Customer>();

const resolvers = {
  Query: {
    invoice: (_parent: unknown, args: { id: string }): Invoice | null => invoices.get(args.id) ?? null,
    settings: (): Settings => ({ prefix: 'INV' }),
    customer: (_parent: unknown, args: { id: string }): Customer | null => customers.get(args.id) ?? null,
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
