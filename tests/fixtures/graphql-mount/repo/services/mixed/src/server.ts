import web from "fixture-web";
import { serveRpc } from "fixture-procs";
import { createGqlServer } from "fixture-gql-server";
import { appRouter } from "./kit";
import { buildParts } from "./parts";

const app = web();

// A procedure adapter served under the endpoint it is handed.
app.all("/rpc/*", (request, reply) => {
  void serveRpc({ router: appRouter, endpoint: "/rpc", request }).then((result) =>
    reply.send(result),
  );
});

// A schema bound by destructuring, handed to a GraphQL server: no
// procedure walk reads it.
const { schema } = buildParts();
app.use("/graphql", createGqlServer({ schema }));
