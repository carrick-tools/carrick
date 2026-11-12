import { Hono } from 'hono';
import { createYoga } from 'graphql-yoga';
import { GraphQLID, GraphQLInt, GraphQLNonNull, GraphQLObjectType, GraphQLSchema, GraphQLString } from 'graphql';

// The schema is built here, in code. A build step prints it to
// `dist/schema.graphql`, which is committed.
interface ParcelRecord {
  id: string;
  weightGrams: number | null;
  etaDays: number | null;
}

interface DepotRecord {
  code: string;
  city: string | null;
}

const parcels = new Map<string, ParcelRecord>();
const depots = new Map<string, DepotRecord>();

const Parcel = new GraphQLObjectType({
  name: 'Parcel',
  fields: {
    id: { type: new GraphQLNonNull(GraphQLID) },
    weightGrams: { type: GraphQLInt },
    etaDays: { type: GraphQLInt },
  },
});

const Depot = new GraphQLObjectType({
  name: 'Depot',
  fields: {
    code: { type: new GraphQLNonNull(GraphQLString) },
    city: { type: GraphQLString },
  },
});

const Query = new GraphQLObjectType({
  name: 'Query',
  fields: {
    parcel: {
      type: Parcel,
      args: { id: { type: new GraphQLNonNull(GraphQLID) } },
      resolve: (_parent: unknown, args: { id: string }): ParcelRecord | null => parcels.get(args.id) ?? null,
    },
    depot: {
      type: Depot,
      args: { code: { type: new GraphQLNonNull(GraphQLString) } },
      resolve: (_parent: unknown, args: { code: string }): DepotRecord | null => depots.get(args.code) ?? null,
    },
  },
});

const yoga = createYoga({ schema: new GraphQLSchema({ query: Query }) });

const app = new Hono();

app.get('/health', (c) => c.json({ ok: true }));

app.all('/graphql', (c) => yoga.fetch(c.req.raw));

export default app;
