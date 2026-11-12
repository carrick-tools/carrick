import router, { Ctx } from "fixture-mw-router";

function ok(ctx: Ctx): void {
  ctx.body = "ok";
}

export const items = router();
items.post("/items.list", ok);
items.post("/items.add", ok);
items.post("/items.remove", ok);
