import { createSchema } from 'graphql-yoga';
import { readFileSync } from 'node:fs';

export const schema = createSchema({
  typeDefs: readFileSync(new URL('../../../tooling/graphql/dist/catalog.graphql', import.meta.url), 'utf8'),
  resolvers: {},
});
