#!/usr/bin/env node
// The detached usage send: `node usage-run.(ts|js) <workspace root>`.
//
// Started by `startUsageSend` (usage.ts) from the session-start hook. Prints
// nothing and exits 0, whatever happened.

import { createLogger } from "../log.ts";
import { sendUsage } from "./usage.ts";

const log = createLogger("carrick-usage");

try {
  const root = process.argv[2];
  if (root) log(`usage send: ${await sendUsage(root)}`);
} catch (error) {
  log("usage send failed", String(error));
}
process.exit(0);
