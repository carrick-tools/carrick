import { t } from "./kit";

const other = (record: Record<string, unknown>) => record;

// The alias may be set again, so the call through it names nothing.
let mk = t.router;
mk = other as typeof t.router;

export const letRouter = mk({
  ping: t.procedure.query(() => "pong"),
});
