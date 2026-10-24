import { t } from "./kit";
import { healthProcedure } from "./health";
import { ordersRouter } from "./orders";
import { looseRouter } from "./loose";

export const appRouter = t.router({
  ping: t.procedure.query(() => "pong"),
  health: healthProcedure(),
  orders: ordersRouter,
  loose: looseRouter,
  // A member the label states no method for: no HTTP operation.
  events: t.procedure.stream(() => "tick"),
});
