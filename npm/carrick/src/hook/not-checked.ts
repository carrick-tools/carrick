// The one line an edit gets when the compiler engine did not check it (T7,
// #4654): it names the service and why, once per session, service and state.
// Agent-facing. A late answer is filed and carried, and a read is never
// told, so only the edit hook calls this.

import { rootKey } from "../held/paths.ts";
import type { NotChecked } from "../held/state.ts";
import { wasTold } from "./reuse.ts";

const HEAD = "Carrick: the compiler engine did not check this edit of";
const TAIL = "`carrick doctor` lists what is not checked.";
// A worktree is not a state the doctor lists, so its line has no doctor tail (T2, #5038).

function why(note: NotChecked): string {
  switch (note.state) {
    case "cold":
      return note.took_s === undefined ? "it is starting" : `it is starting, which took about ${note.took_s} s last time`;
    case "capped":
      return `the memory budget of ${note.budget_mb ?? 0} MB is held by ${note.holders ?? 0} other engines`;
    case "no_compiler":
      return "no TypeScript 7 compiler is installed";
    case "error":
      return note.reason === undefined ? "it could not start" : `it could not start: ${note.reason.replace(/[.\s]+$/, "")}`;
    case "worktree":
      return worktreeWhy(note.reason);
  }
}

/**
 * Why a worktree edit was not checked, by the refusal the daemon carried (T2 F6, #5038). Proposed wording: it goes through the
 * wording pass before it ships. No reason: the service's folders are not under the main checkout, so no worktree copy is made.
 */
function worktreeWhy(reason: string | undefined): string {
  switch (reason) {
    case "worktree_deps_differ":
      return "the file is in a git worktree whose package or config files differ from the main checkout";
    case "vendored_repo_file_differs":
      return "the file is in a git worktree that changed a file the service reads as an installed package";
    case "vendored_reaches_own":
      return "the file is in a git worktree, and an installed package imports the service's own files";
    case "no_saved_answer":
    case "stale_build":
    case "stale_main_inputs":
      return "the file is in a git worktree, and the main checkout's compiler engine could not be started first";
    case "worktree_unreadable":
      return "the file is in a git worktree that could not be read";
    case "destination_in_main":
      return "the file is in a git worktree whose engine folder would sit inside the main checkout's";
    default:
      return "the file is in a git worktree outside the folders the main checkout's engine reads";
  }
}

/** The lines to print now and the keys to remember once they are delivered. */
export function notCheckedLines(session: string, root: string, notes: readonly NotChecked[], home?: string): { lines: string[]; keys: string[] } {
  const lines: string[] = [];
  const keys: string[] = [];
  for (const note of notes) {
    // A second worktree of the same session is a new line.
    const key = `held-not-checked::${rootKey(root)}::${note.service}::${note.state}${note.state === "worktree" ? `::${note.checkout ?? ""}` : ""}`;
    if (keys.includes(key) || wasTold(session, key, home)) continue;
    keys.push(key);
    lines.push(`${HEAD} ${note.service}: ${why(note)}.${note.state === "worktree" ? "" : ` ${TAIL}`}`);
  }
  return { lines, keys };
}
