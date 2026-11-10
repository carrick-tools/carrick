import { t } from "./kit";

// A second router that also holds `shelf.list`.
export const adminRouter = t.router({
  shelf: t.router({
    list: t.procedure.query(() => ["admin"]),
  }),
});

export type AdminRouter = typeof adminRouter;
