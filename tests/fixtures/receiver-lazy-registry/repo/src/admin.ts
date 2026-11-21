import router, { App, Reply, Request } from "fixture-router";

function stats(_request: Request, reply: Reply): void {
  reply.send("stats");
}

export default function init(app: App = router()): void {
  app.get("/reg/stats", stats);
}
