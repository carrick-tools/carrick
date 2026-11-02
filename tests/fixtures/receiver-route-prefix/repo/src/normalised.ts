import router, { Reply, Request } from "fixture-router";

declare const process: { env: { [key: string]: string | undefined } };

function ok(_request: Request, reply: Reply): void {
  reply.send("ok");
}

export function start(configured: string | undefined): void {
  // A base normalised in steps, each a conditional on the one before.
  const raw = process.env.BASE_PATH ?? configured;
  const normalised = raw && raw !== "/" ? "/" + raw.replace(/^\//, "") : undefined;
  const base: string | undefined = normalised === "/" ? undefined : normalised;
  const app = router();
  const prefix = base ? `${base}v2` : "/v2";
  app.get(prefix + "/status", ok);
}
