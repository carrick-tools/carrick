import { createGqlServer } from "fixture-gql-server";
import { schema } from "./schema";

// A GraphQL server handed to nothing that mounts it: it serves at the
// library's default endpoint, which the code never writes.
export const handler = createGqlServer({ schema });
