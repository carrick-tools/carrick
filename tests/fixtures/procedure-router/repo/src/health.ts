import { t } from "./kit";

// A procedure a local function builds and returns.
export function healthProcedure() {
  return t.procedure.query(() => "ok");
}
