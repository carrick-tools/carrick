// The detached child that upgrades an existing global install.
//
// Started by the session-start hook (`src/hook/upgrade.ts`), which does not
// wait for it. It prints nothing on either stream: its parent is gone and its
// stdio is `ignore`. What it did is read by the next session from
// `self-upgrade.json` (`src/upgrade.ts`).

import { runUpgrade } from "./upgrade.ts";

await runUpgrade().catch(() => {});
process.exit(0);
