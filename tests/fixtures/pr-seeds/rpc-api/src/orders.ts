import { t } from "./kit";

export type OrderSummary = {
  id: string;
  total: number;
};

const placed: OrderSummary[] = [{ id: "1", total: 1200 }];

export const ordersRouter = t.router({
  list: t.procedure.query((): OrderSummary[] => placed),
  create: t.procedure.input({ id: "string" }).mutation(() => ({ id: "2" })),
});
