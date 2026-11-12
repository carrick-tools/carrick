import router, { Ctx } from "fixture-mw-router";
import createApp from "fixture-mw-app";

function ok(ctx: Ctx): void {
  ctx.body = "ok";
}

const sub = router();
sub.post("/s.list", ok);

// An app and a router made on one line are two instances, not one.
const app = createApp(); const outer = router();
outer.use("/v1", sub.routes());
app.use(outer.routes());
