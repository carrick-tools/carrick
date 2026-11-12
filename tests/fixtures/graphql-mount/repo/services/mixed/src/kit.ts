import { init } from "fixture-procs";

export const t = init.create();

export const appRouter = t.router({
  ping: t.procedure.query(() => "pong"),
});
