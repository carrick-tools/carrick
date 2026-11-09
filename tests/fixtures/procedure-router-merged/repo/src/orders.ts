import { router } from "./merged-alias";
import { t } from "./kit";

export const ordersRouter = router({
  get: t.procedure.query(() => "order"),
});
