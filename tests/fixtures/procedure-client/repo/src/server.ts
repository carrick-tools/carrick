import web from "fixture-web";
import { serveRpc } from "fixture-procs";
import { appRouter } from "./router";
import { adminRouter } from "./admin";

const app = web();

app.all("/api/rpc/*", (request, reply) => {
  void serveRpc({ router: appRouter, endpoint: "/api/rpc", request }).then((result) =>
    reply.send(result),
  );
});

app.all("/admin/rpc/*", (request, reply) => {
  void serveRpc({ router: adminRouter, endpoint: "/admin/rpc", request }).then((result) =>
    reply.send(result),
  );
});
