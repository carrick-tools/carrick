import router, { App, Reply, Request } from "fixture-router";

function handle(_request: Request, reply: Reply): void {
  reply.send("c");
}

export default function init(app: App = router()): void {
  app.get("/c/ping", handle);
}
