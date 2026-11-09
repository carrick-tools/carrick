import { t } from "./kit";

// The router member is called directly: the control that already reads.
export const directRouter = t.router({
  ping: t.procedure.query(() => "pong"),
});
