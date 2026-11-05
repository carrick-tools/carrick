#!/usr/bin/env node
// The background re-check runner: `carrick hook recheck-run <workspace root>`.
//
// Started detached by `ensureRunner` (rechecks.ts) from a hook that queued an
// edit; it works the workspace's queue to empty, one `carrick check --recheck`
// at a time, and exits. It prints nothing: answers go to session files that the
// next hook delivers. It exits 0 whatever happens.

import { createLogger } from "../log.ts";
import { runQueue } from "./rechecks.ts";

const log = createLogger("carrick-recheck-run");

async function main(): Promise<void> {
  // `carrick hook recheck-run <root>`: the bin script leaves `hook` and the name in argv.
  const root = process.argv.slice(2).find((arg) => arg !== "hook" && arg !== "recheck-run");
  if (!root) {
    log("no workspace root given");
    return;
  }
  const spawner = Number.parseInt(process.env["CARRICK_RECHECK_SPAWNER"] ?? "", 10);
  await runQueue(root, Number.isFinite(spawner) ? { spawner } : {});
}

try {
  await main();
} catch (error) {
  log("runner failed", String(error));
}
process.exit(0);
