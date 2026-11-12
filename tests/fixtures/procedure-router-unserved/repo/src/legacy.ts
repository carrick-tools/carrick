import web from "fixture-web";
import { rpcMiddleware } from "fixture-procs-mw";
import { t } from "./kit";

// A router held in a binding that may be set again: the walk stops there.
export let legacyRouter = t.router({
  old: t.procedure.query(() => "old"),
});

const app = web();
app.use("/d", rpcMiddleware({ router: t.router({ legacy: legacyRouter }) }));
