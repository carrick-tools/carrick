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
      return "the file is in a git worktree, and the engine reads only the main checkout";
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
