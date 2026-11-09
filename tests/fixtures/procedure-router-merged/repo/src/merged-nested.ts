import { t } from "./kit";

// A merge under a key: both routers serve beneath it.
export const nestedMerge = t.router({
  admin: t.mergeRouters(
    t.router({ x: t.procedure.query(() => "x") }),
    t.router({ y: t.procedure.query(() => "y") }),
  ),
});
