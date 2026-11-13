import web from "fixture-web";
import { serveRpc } from "fixture-procs";
import { appRouter } from "./router";

const app = web();

app.all("/api/rpc/*", (request, reply) => {
  void serveRpc({ router: appRouter, endpoint: "/api/rpc", request }).then((result) =>
    reply.send(result),
  );
});
