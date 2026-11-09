#!/usr/bin/env node
// PostToolUse hook for a shell command, on Claude Code.
//
// Most reads and writes of a source file in an agent session go through the
// shell, where the Read and Edit hooks cannot fire. This decides in one process
// what the command touched. A read of a TypeScript file gets the Read hook's
// answer (who depends on it). A write of one (a redirect, `tee`, `sed -i`, a
// script that rewrites it, an `apply_patch`) goes through the post-edit flow
// (`edit-flow.ts`): the same answer an Edit gives, a finding or nothing.
//
// A write is a path the command names in a write shape whose mtime moved since
// this session last saw it. A path with no record counts when it was modified
// in the last two minutes (a file the command created, or rewrote just now).
// Commands that move the tree in bulk (`git checkout`, `npm ci`) start nothing.
//
// It runs on every shell command, so the work before a target is found is a
// node start, a stdin read and one regex: nothing else is imported. A command
// that names no source file prints nothing and starts nothing. Whatever
// happens it exits 0 and prints at most the one JSON object.

import { readStdin } from "./stdin.ts";
import path from "node:path";
import fs from "node:fs";
import { bashTargets, MENTIONS_SOURCE, movesTreeInBulk } from "./bash-targets.ts";

/** At most this many files are asked about for one command. */
const MAX_FILES = 2;
/** At most this many written files go through the edit flow for one command. */
const MAX_WRITES = 5;
/** A written file with no record counts when modified this recently. */
const FRESH_MS = 120_000;

type Payload = {
  cwd?: string;
  session_id?: string;
  tool_input?: { command?: string };
};

async function main(): Promise<void> {
  let payload: Payload;
  try {
    payload = JSON.parse((await readStdin()) || "{}") as Payload;
  } catch {
    return;
  }
  const command = payload.tool_input?.command;
  if (!command || !MENTIONS_SOURCE.test(command)) return;
  const cwd = payload.cwd ?? process.cwd();
  const { reads, writes } = bashTargets(command, path.resolve(cwd));
  const candidates = writes.length > 0 && !movesTreeInBulk(command, path.resolve(cwd)) ? writes : [];
  if (reads.length === 0 && candidates.length === 0) return;

  const session = payload.session_id;
  const { markInvoker } = await import("./invoker.ts");
  const { resolveChannel } = await import("../channel.ts");
  const channel = resolveChannel({ hooksInstalled: true });
  const reuse = await import("./reuse.ts");

  // Written: the mtime moved since the session last saw the file.
  const written: string[] = [];
  if (candidates.length > 0 && channel.channel !== "off") {
    const mtimes: Record<string, number> = {};
    for (const file of candidates) {
      let mtime: number;
      try {
        mtime = fs.statSync(file).mtimeMs;
      } catch {
        continue;
      }
      mtimes[file] = mtime;
      const seen = session ? reuse.writtenMtime(session, file) : undefined;
      if (seen === undefined ? Date.now() - mtime < FRESH_MS : mtime !== seen) written.push(file);
    }
    if (session && Object.keys(mtimes).length > 0) reuse.markWritten(session, mtimes);
  }
  const toEdit = written.slice(0, MAX_WRITES);

  // The answer is the Read hook's (or the edit hook's), so the cloud's log tells it the same way.
  markInvoker(toEdit.length > 0 ? "post-edit" : "post-read");
  if (toEdit.length === 0 && channel.channel !== "hook") return;
  const { wasRead } = reuse;
  const fresh =
    channel.channel === "hook" ? reads.filter((file) => !(session && wasRead(session, file))).slice(0, MAX_FILES) : [];
  const { answerRead } = await import("./read-answer.ts");
  const answers = (await Promise.all(fresh.map((file) => answerRead(file, payload.cwd, session)))).filter(
    (line): line is string => line !== null,
  );

  if (toEdit.length > 0) {
    const { editFlow } = await import("./edit-flow.ts");
    await editFlow({ cwd: payload.cwd ?? null, session, channel: channel.channel }, toEdit, answers);
    return;
  }
  const { deliverLate } = await import("./late.ts");
  const contexts = [...deliverLate(session), ...answers];
  if (contexts.length === 0) return;
  process.stdout.write(
    JSON.stringify({
      hookSpecificOutput: { hookEventName: "PostToolUse", additionalContext: contexts.join("\n\n") },
    }),
  );
}

await main();
process.exit(0);
