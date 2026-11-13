import web from "fixture-web";
import { createGqlServer } from "fixture-gql-server";
import { schema } from "./schema";

const app = web();

// A GraphQL server mounted under a path.
app.use("/graphql", createGqlServer({ schema }));
