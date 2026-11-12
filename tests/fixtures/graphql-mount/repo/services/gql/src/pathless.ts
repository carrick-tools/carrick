import web from "fixture-web";
import { createGqlServer } from "fixture-gql-server";
import { schema } from "./schema";

const app = web();

// A GraphQL server the host's verified mount member takes with no path:
// the endpoint is the library's own, so it is unread, never "/".
app.mount(createGqlServer({ schema }));
