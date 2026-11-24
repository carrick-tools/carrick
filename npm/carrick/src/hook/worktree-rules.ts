// The rules file for an agent in a linked git worktree (carrick#5134). `carrick
// init` writes `.carrick/RULES.md` and git does not track it, so a fresh linked
// worktree has the AGENTS.md line that points at it and no file. The first hook
// event of the session there says once where the main checkout's copy is. A
// sub-agent gets no session start of its own, so the Read and Edit hooks carry
// the same line. Nothing is written in the worktree: a stray `.carrick/` would
// make a nested worktree read as a workspace with no index.

import fs from "node:fs";
import path from "node:path";
import { wasTold } from "./reuse.ts";
import { linkedCheckoutTop, mainCheckoutOf } from "./worktree.ts";

/** The line, and the key to remember once it is delivered; null when there is nothing to say. */
export function worktreeRulesLine(
  session: string | undefined,
  cwd: string | null | undefined,
  home?: string,
): { line: string; key: string } | null {
  if (!session || !cwd) return null;
  const top = linkedCheckoutTop(path.join(cwd, "_"));
  if (top === null || fs.existsSync(path.join(top, ".carrick", "RULES.md"))) return null;
  const main = mainCheckoutOf(top);
  if (main === null) return null;
  const rules = path.join(main, ".carrick", "RULES.md");
  if (!fs.existsSync(rules)) return null;
  const key = `worktree-rules::${top}`;
  if (wasTold(session, key, home)) return null;
  return { line: `Carrick: this worktree has no copy of Carrick's rules; read ${rules} instead.`, key };
}
