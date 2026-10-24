import { t } from "./kit";

const orderInput = { id: "string" };

// A nested router, held in a binding another module imports.
export const ordersRouter = t.router({
  list: t.procedure.query(() => []),
  create: t.procedure.input(orderInput).mutation(() => ({ id: "1" })),
});
