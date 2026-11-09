import { t } from "./kit";

const list = [t.router({ z: t.procedure.query(() => "z") })];

// Control: a spread argument names no router, so no procedure is read.
export const spreadMerge = t.mergeRouters(...list);
