import { t } from "./kit";
import { mergeRouters, router } from "./merged-alias";
import { ordersRouter } from "./orders";

// One argument is written here, the other is a const in another module.
export const aliasedMerge = mergeRouters(
  router({ list: t.procedure.query(() => []) }),
  ordersRouter,
);
