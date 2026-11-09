// The Read hook's answer for one file: who depends on it, rendered as
// locations only. Shared by `hook post-read` (a Read of the file) and
// `hook post-bash` (a shell command that reads it).

import path from "node:path";
import { check } from "../cli.ts";
import { createLogger } from "../log.ts";
import { renderPostRead } from "../render.ts";
import { resolveRoot, rootNote } from "../root.ts";
import { markRead, markTold, toldKey } from "./reuse.ts";

const log = createLogger("carrick-hook");
/** The limit on the one check, unless the caller set its own. */
const CHECK_TIMEOUT_MS = "2000";

/**
 * The line for `file` (absolute), or null when there is nothing to say. The
 * file is marked as asked about whatever the answer was, so an error or a
 * timeout is not asked again.
 */
export async function answerRead(
  file: string,
  cwd: string | undefined,
  session: string | undefined,
): Promise<string | null> {
  const choice = resolveRoot({
    clientRoot: cwd ?? null,
    projectDir: process.env["CLAUDE_PROJECT_DIR"] ?? null,
    filePath: file,
  });
  const note = rootNote(choice);
  if (note) log(note);

  const relative = path.relative(choice.root, file);
  const env = { ...process.env, CARRICK_TIMEOUT_MS: process.env["CARRICK_TIMEOUT_MS"] ?? CHECK_TIMEOUT_MS };
  const outcome = await check(relative, { cwd: choice.root, env });
  if (session) markRead(session, file);
  if (!outcome.result) {
    log("no answer for", relative, outcome.failure ?? "");
    return null;
  }

  const context = renderPostRead(outcome.result, relative);
  log(`check ${relative} -> ${context ? "context" : "nothing to say"} in ${outcome.ms}ms`);
  // The post-edit hook that follows prints this file's header and items but
  // not the uses lines it was just shown.
  if (session && outcome.result.uses_lines?.length) {
    markTold(session, [toldKey(outcome.result, relative)]);
  }
  return context;
}
