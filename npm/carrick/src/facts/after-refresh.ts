// What `carrick refresh` leaves behind besides the index: the fact file of the
// project the workspace is set up for (carrick#5094). The refresh rewrites the
// hosted snapshot, which is where the fetch reads its project, so the fetch
// that follows asks for the right one; a file kept from another project is
// replaced there (`refreshFacts`), and the hooks' cooldown is not in this path.

import path from "node:path";
import { withQuietLog } from "../log.ts";
import { fetchFactsNow } from "./fetch.ts";

/** The workspace a `carrick refresh` invocation ran in: its `--workspace`, else the folder it ran from. */
function workspaceOf(args: string[]): string {
  const at = args.indexOf("--workspace");
  const given = at >= 0 ? args[at + 1] : undefined;
  return path.resolve(given && !given.startsWith("-") ? given : process.cwd());
}

/** Fetch the fact file after a refresh that succeeded, saying nothing. Never throws. */
export async function factsAfterRefresh(args: string[], options: Parameters<typeof fetchFactsNow>[1] = {}): Promise<void> {
  try {
    await withQuietLog(() => fetchFactsNow(workspaceOf(args), options));
  } catch {
    // The hooks' own refresh brings the file instead.
  }
}
