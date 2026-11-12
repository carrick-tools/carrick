import { rpcMiddleware } from "fixture-procs-mw";
import { t } from "./kit";

export const mountedRouter = t.router({
  mounted: t.procedure.query(() => "mounted"),
});

// An in-repo function no label mounts: the adapter's result goes to a call
// nothing verifies.
function register(handler: unknown): void {
  void handler;
}

register(rpcMiddleware({ router: mountedRouter }));
