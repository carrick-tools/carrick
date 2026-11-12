import router from "fixture-mw-router";
import handlerRouter, { Request, Response } from "fixture-hrouter";

function ok(_request: Request, response: Response): void {
  response.send("ok");
}

// A router of another package handed past a path.
const outer = router();
const inner = handlerRouter();
inner.post("/c", ok);
outer.use("/v2", inner);
