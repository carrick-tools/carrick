import router, { Reply, Request } from "fixture-router";

declare const process: { env: { [key: string]: string | undefined } };

function ok(_request: Request, reply: Reply): void {
  reply.send("ok");
}

export function serve(): void {
  // A deployment base read at runtime: unset, the routes are served at the root.
  const base: string | undefined = process.env.BASE_PATH;
  const app = router();
  // A prefix chosen by a conditional, one way written whole: served there.
  const apiPrefix = base ? `${base}api` : "/api";
  app.get(apiPrefix + "/session", ok);
  app.post(`${apiPrefix}/session/setup`, ok);
  // A default with a literal fallback.
  app.get(base ?? "/", ok);
  // A literal route.
  app.get("/health", ok);
  // A base with no way written whole: no path is stated.
  app.get(`${base}health`, ok);
}
