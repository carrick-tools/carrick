import { rpcMiddleware } from "fixture-procs-mw";
import { t } from "./kit";

export const bareRouter = t.router({
  bare: t.procedure.query(() => "bare"),
});

// A module-level adapter: no prefix and no host call.
export const bareHandler = rpcMiddleware({ router: bareRouter });
