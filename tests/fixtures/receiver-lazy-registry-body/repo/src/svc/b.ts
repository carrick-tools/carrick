import router, { App, Reply, Request } from "fixture-router";

function handle(_request: Request, reply: Reply): void {
  reply.send("b");
}

export default function init(app: App = router()): void {
  app.get("/b/ping", handle);
}
