import { t } from "./kit";

export const appRouter = t.router({
  ping: t.procedure.query(() => "pong"),
});
