import { t } from "./kit";

const countBooks = t.procedure.query(() => 0);

export const appRouter = t.router({
  shelf: t.router({
    list: t.procedure.query(() => ["a"]),
    add: t.procedure.mutation(() => ({ id: "1" })),
    inner: t.router({
      get: t.procedure.query(() => "one"),
    }),
  }),
  count: countBooks,
});

export type AppRouter = typeof appRouter;
