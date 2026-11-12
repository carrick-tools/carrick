import { t } from "./kit";

export const appRouter = t.router({
  shelf: t.router({
    list: t.procedure.query(() => ["a"]),
    add: t.procedure.mutation(() => ({ id: "1" })),
    drawer: t.router({
      items: t.procedure.query(() => ["b"]),
    }),
  }),
});

export type AppRouter = typeof appRouter;
