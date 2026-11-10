import { t } from "./kit";

// Two routers in one file that share a first key.
export const firstRouter = t.router({
  shared: t.procedure.query(() => "one"),
});

export const secondRouter = t.router({
  shared: t.procedure.query(() => "two"),
});
