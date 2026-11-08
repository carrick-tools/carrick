import router, { Reply, Request } from "fixture-router";

declare const process: { env: { [key: string]: string | undefined } };

function ok(_request: Request, reply: Reply): void {
  reply.send("ok");
}

export function serve(): void {
  // A deployment base read at runtime, with no literal fallback.
  const basePath: string | undefined = process.env.BASE_PATH;
  const app = router();
  // The base joined with text: no way is written whole.
  app.get(`${basePath}health`, ok);
  // The base rewritten by a call: no way is written whole.
  app.post(String(basePath).replace(/\/$/, "") + "/jobs", ok);
  // A default held in a local: served at the default (carrick#3191).
  const root = basePath ?? "/";
  app.get(root, ok);
  // A literal route beside them.
  app.get("/ready", ok);
}
