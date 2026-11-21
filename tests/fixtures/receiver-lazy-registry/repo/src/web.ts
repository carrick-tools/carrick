import router, { App, Reply, Request } from "fixture-router";

function ping(_request: Request, reply: Reply): void {
  reply.send("pong");
}

export default function init(app: App = router()): void {
  app.post("/reg/ping", ping);
}
