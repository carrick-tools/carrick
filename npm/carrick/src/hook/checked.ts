// Whether `carrick check --changed` already ran since this session's last edit.
//
// The hooks' lines end "Run `carrick check --changed` before you finish."
// An agent that had just run it ran it again, 137 s and then 133 s
// (carrick#3357). The binary leaves a marker in the workspace when that
// command ends (`record_check_marker` in `src/local_mode/changed.rs`, same
// file name and fields); the hooks compare it with the newest edit the
// session record holds (`recordEdit` in `reuse.ts`).

import fs from "node:fs";
import path from "node:path";
import { readSession } from "./reuse.ts";

/** The marker the binary writes, atomically, under `<root>/.carrick/`. */
export function checkMarkerPath(root: string): string {
  return path.join(root, ".carrick", "last-check-changed.json");
}

type Marker = { started_at_ms: number; finished_at_ms: number; exit: number };

function readMarker(root: string): Marker | null {
  try {
    const parsed = JSON.parse(fs.readFileSync(checkMarkerPath(root), "utf8")) as Partial<Marker>;
    if (typeof parsed.started_at_ms !== "number" || typeof parsed.exit !== "number") return null;
    return parsed as Marker;
  } catch {
    return null;
  }
}

/**
 * True when a `carrick check --changed` that exited 0 began after this
 * session's newest edit in the workspace at `root`: it read the tree as it
 * stands, so the clause would only send the agent to repeat it. Exit 1 (a
 * problem to fix) and 2 (nothing checked) never count, and neither does a
 * session with no recorded edit (nothing to compare). Started rather than
 * finished: an edit made while the check ran may not have been read. A Stop
 * that stated every edited file checked (`covered` in the session record)
 * settles it the same way.
 */
export function checkedSinceLastEdit(session: string | undefined, root: string, home?: string): boolean {
  if (!session) return false;
  const record = readSession(session, home);
  const edited = record.edited[root];
  if (edited === undefined) return false;
  // The Stop hook stated that the per-file re-checks answered every file this
  // session edited, up to its newest edit (carrick#3466): the same settlement.
  if ((record.covered[root] ?? -1) >= edited) return true;
  const marker = readMarker(root);
  return marker !== null && marker.exit === 0 && marker.started_at_ms > edited;
}
