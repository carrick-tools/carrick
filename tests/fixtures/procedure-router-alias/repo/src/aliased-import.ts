import { t } from "./kit";
import { createRouter } from "./aliased-kit";

export const importedRouter = createRouter({
  list: t.procedure.query(() => []),
  nested: createRouter({
    get: t.procedure.query(() => "one"),
  }),
});
