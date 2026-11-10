import { t } from "./kit";

export const appRouter = t.router({
  shelf: t.router({
    list: t.procedure.query(() => [{ id: "1", title: "Dune" }]),
    get: t.procedure.query(() => ({ id: "1", title: "Dune" })),
    add: t.procedure.input({ title: "string" }).mutation(() => ({ id: "2" })),
  }),
});

export type AppRouter = typeof appRouter;
