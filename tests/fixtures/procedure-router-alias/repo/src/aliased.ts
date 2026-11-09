import { t } from "./kit";

// The router member is called through a `const` alias in the same module.
const mk = t.router;

export const aliasedRouter = mk({
  ping: t.procedure.query(() => "pong"),
});
