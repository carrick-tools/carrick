import { t } from "./kit";

// Two routers merged by the member: both serve at the one level.
export const mergedRouter = t.mergeRouters(
  t.router({ a: t.procedure.query(() => "a") }),
  t.router({ b: t.procedure.mutation(() => "b") }),
);
