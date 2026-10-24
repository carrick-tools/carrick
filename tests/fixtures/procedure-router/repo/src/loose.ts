import { t } from "./kit";

const extra = {
  hidden: t.procedure.query(() => "hidden"),
};
const dynamicKey = "dynamic";

// A key written before a spread or a computed key may be set again by it:
// only the key written after both is read.
export const looseRouter = t.router({
  kept: t.procedure.query(() => "kept"),
  ...extra,
  [dynamicKey]: t.procedure.query(() => "dynamic"),
  after: t.procedure.query(() => "after"),
});

// A router no adapter serves.
export const idleRouter = t.router({
  idle: t.procedure.query(() => "idle"),
});
