import web from "fixture-web";
import { rpcMiddleware } from "fixture-procs-mw";
import { t } from "./kit";

const late = "late";

// A computed key: the key written before it may be set again by it.
export const computedRouter = t.router({
  before: t.procedure.query(() => "before"),
  [late]: t.procedure.query(() => "late"),
});

const app = web();
app.use("/c", rpcMiddleware({ router: computedRouter }));
