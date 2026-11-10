import { t } from "./kit";

// The keys sit in a table another binding hands to the router.
const routes = {
  beta: t.procedure.query(() => "b"),
};

export const tableRouter = t.router(routes);
