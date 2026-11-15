import router, { App, Reply, Request } from "fixture-router";

function handle(_request: Request, reply: Reply): void {
  reply.send("e");
}

export default function init(app: App = router(), _server?: unknown): void {
  app.get("/e/ping", handle);
}
