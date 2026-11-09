import router, { Reply, Request } from "fixture-router";

declare const process: { env: { [key: string]: string | undefined } };

function ok(_request: Request, reply: Reply): void {
  reply.send("ok");
}

// The same defaults, held in module constants.
const app = router();
const { sprocketsAt = "/sprockets" }: { sprocketsAt?: string } = JSON.parse("{}");
const springsAt = process.env.SPRINGS_AT ?? "/springs";
app.get(sprocketsAt, ok);
app.get(`${springsAt}/:springId`, ok);

export default app;
