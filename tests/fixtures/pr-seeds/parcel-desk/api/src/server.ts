import { Hono } from 'hono';
import { createYoga } from 'graphql-yoga';
import { GraphQLID, GraphQLInt, GraphQLNonNull, GraphQLObjectType, GraphQLSchema } from 'graphql';

// The schema is built here, in code. A build step prints it to
// `dist/schema.graphql`, which is committed.
interface ConsignmentRecord {
  id: string;
  weightGrams: number | null;
  total: number | null;
}

const consignments = new Map<string, ConsignmentRecord>();

const Consignment = new GraphQLObjectType({
  name: 'Consignment',
  fields: {
    id: { type: new GraphQLNonNull(GraphQLID) },
    weightGrams: { type: GraphQLInt },
    total: { type: GraphQLInt },
  },
});

const Query = new GraphQLObjectType({
  name: 'Query',
  fields: {
    consignment: {
      type: Consignment,
      args: { id: { type: new GraphQLNonNull(GraphQLID) } },
      resolve: (_parent: unknown, args: { id: string }): ConsignmentRecord | null => consignments.get(args.id) ?? null,
    },
  },
});

const yoga = createYoga({ schema: new GraphQLSchema({ query: Query }) });

const app = new Hono();

app.get('/health', (c) => c.json({ ok: true }));

app.all('/graphql', (c) => yoga.fetch(c.req.raw));

export default app;
