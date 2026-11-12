#!/usr/bin/env node
// The detached fact refresh: `node refresh-run.(ts|js) <workspace root>`.
//
// Started by `startFactsRefresh` (fetch.ts) from the session-start hook or a
// hook that found the fact file stale. Prints nothing and exits 0.

import { createLogger } from "../log.ts";
import { runFactsRefresh } from "./fetch.ts";

const log = createLogger("carrick-facts");

try {
  const root = process.argv[2];
  if (root) await runFactsRefresh(root);
} catch (error) {
  log("fact refresh failed", String(error));
}
process.exit(0);
