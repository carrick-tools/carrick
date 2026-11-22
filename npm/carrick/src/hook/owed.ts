// Read lines owed to a session that read files before the workspace had a
// fact file.
//
// A new workspace has no fact file until its first refresh lands, and the cloud
// builds the file on the first ask (`facts/fetch.ts`). A Read in that window is
// answered by the binary's `check` alone and the file is marked as asked about,
// so its fact lines (who calls its routes and functions, the chain line) were
// never said: the agent read the file once and edited it without them. Each
// such file is recorded here (`markOwed`), and the first Read, shell read or
// edit after the fact file lands says its Read lines once, under the same cap
// and the same said-once claims as any other fact line.

import { chainLinesEnabled, contractLinesEnabled, readLinesEnabled } from "../facts/enabled.ts";
import type { FactOutcome } from "../facts/lines.ts";
import { clearOwed, factsOwed } from "./reuse.ts";

/**
 * The owed Read lines of `session` in `root`, or null while nothing is owed
 * there, the fact file is still missing, or `limit` leaves no room. `skip` names files this action
 * answers itself. A file is owed once: it is cleared whether or not a line
 * printed for it.
 */
export async function owedLines(
  root: string,
  session: string,
  options: { limit: number; exclude: ReadonlySet<string>; skip?: readonly string[] },
): Promise<FactOutcome | null> {
  // No room under this action's cap: the lines stay owed for the next one.
  if (options.limit <= 0) return null;
  const owed = factsOwed(session, root);
  if (owed.length === 0) return null;
  const files = owed.filter((file) => options.skip?.includes(file) !== true);
  const { factLines } = await import("../facts/lines.ts");
  const outcome = await factLines(
    root,
    files.map((file) => ({ file, read: true })),
    session,
    { limit: options.limit, exclude: options.exclude, contract: contractLinesEnabled(), chain: chainLinesEnabled(), chainOnly: !readLinesEnabled() },
  );
  if (!outcome.present) return null;
  clearOwed(session, owed);
  return outcome;
}
