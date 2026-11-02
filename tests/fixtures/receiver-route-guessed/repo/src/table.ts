import router, { Reply, Request } from "fixture-router";

function ok(_request: Request, reply: Reply): void {
  reply.send("ok");
}

// A table of routes the module writes whole, registered in a loop whose
// paths the compiler types as `string`.
const routes = [
  { path: "/items", handler: ok },
  { path: "/orders", handler: ok },
];

export function start(): void {
  const app = router();
  for (const route of routes) {
    app.get(route.path, route.handler);
  }
}
