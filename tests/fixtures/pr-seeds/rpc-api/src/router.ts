import { t } from "./kit";
import { ordersRouter } from "./orders";

export const appRouter = t.router({
  ping: t.procedure.query(() => "pong"),
  orders: ordersRouter,
});

export type AppRouter = typeof appRouter;
