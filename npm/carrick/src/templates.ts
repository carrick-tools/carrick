// Where the files Carrick ships beside the emit sit: the agent hook launcher
// and the task skill bodies.

import path from "node:path";
import { fileURLToPath } from "node:url";

/**
 * The directory the shipped template files sit in.
 *
 * Resolved from this module rather than from the process, because the same
 * resolution has to hold in the checkout (`src/../templates`) and in the
 * published emit (`dist/../templates`). `init/task-skills.ts` reads the skill
 * bodies from a subdirectory of it.
 */
export function templatesDir(): string {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "templates");
}
