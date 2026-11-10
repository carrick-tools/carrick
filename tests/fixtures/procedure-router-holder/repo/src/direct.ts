import { t } from "./kit";

// A router held by the binding it is declared in.
export const directRouter = t.router({
  alpha: t.procedure.query(() => "a"),
});
