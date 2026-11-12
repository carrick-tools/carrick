import router, { Ctx } from "fixture-mw-router";
import createApp from "fixture-mw-app";
import { items } from "./items";

function ok(ctx: Ctx): void {
  ctx.body = "ok";
}

const api = router();
api.post("/ping", ok);
// A pass-through member's value is its receiver: `items` served under `/v1`.
api.use("/v1", items.routes());

const app = createApp();
// An app that takes middleware only serves the router at its own address.
app.use(api.routes());
