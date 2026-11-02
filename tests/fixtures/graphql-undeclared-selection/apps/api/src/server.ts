import { Hono } from 'hono';
import { createYoga, createSchema } from 'graphql-yoga';
import { readFileSync } from 'node:fs';

// The stored record still carries `etaDays`; the schema stopped exposing it.
interface ParcelRecord {
  id: string;
  weightGrams: number | null;
  status: 'WAITING' | 'MOVING' | 'DELIVERED';
  etaDays: number | null;
}

interface DepotRecord {
  code: string;
  city: string | null;
}

const parcels = new Map<string, ParcelRecord>();
const depots = new Map<string, DepotRecord>();

const resolvers = {
  Query: {
    parcel: (_parent: unknown, args: { id: string }): ParcelRecord | null => parcels.get(args.id) ?? null,
    depot: (_parent: unknown, args: { code: string }): DepotRecord | null => depots.get(args.code) ?? null,
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
