import { t } from "./kit";

// No adapter in the repository is handed this router.
export const orphanRouter = t.router({
  orphan: t.procedure.query(() => "orphan"),
});
