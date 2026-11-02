import web from "fixture-web";
import { serveRpc } from "fixture-procs";
import { appRouter } from "./router";

// Plain HTTP routes, served under the API prefix.
const reports = web();
reports.get("/reports/daily", (_request, reply) => {
  reply.send({ rows: [] });
});

const app = web();
app.use("/api/v1", reports);

// The procedure router, served by a fetch-style adapter under its endpoint.
app.all("/api/trpc/*", (request, reply) => {
  void serveRpc({ router: appRouter, endpoint: "/api/trpc", request }).then((result) =>
    reply.send(result),
  );
});

export default app;
