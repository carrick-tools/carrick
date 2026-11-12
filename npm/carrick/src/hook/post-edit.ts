#!/usr/bin/env node
// PostToolUse hook for a file edit, on either host.
//
// Reads the tool payload on stdin and asks the binary once, from the workspace
// root, with a probe that never starts a re-scan (`probe.ts`). An answer in
// hand prints what the edit needs acted on (`renderPostToolUse`: its problems,
// or why it was not re-checked); an edit that needs a re-scan is queued for the
// background runner (`rechecks.ts`), prints one pending line, and its answer is
// delivered by the next carrier (`late.ts`). Output is `additionalContext`,
// which the host attaches to the tool result. Attaching to the result rather than to the next
// turn is why this channel has no "the run ended on the edit" hole.
//
// Two payload shapes reach it, because the two hosts edit differently
// (carrick#1335). Claude Code's `Edit`, `Write` and `MultiEdit` name one file
// in `tool_input.file_path`. Codex's `apply_patch` carries a patch in
// `tool_input.command`, and one patch can touch several files — so the file is
// a list here, checked in the order the payload names them.
//
// It exits 0 whatever happens. An edit never fails because Carrick had nothing
// to say, could not find its binary, or timed out.

import { readStdin } from "./stdin.ts";
import path from "node:path";
import { resolveChannel } from "../channel.ts";
import fs from "node:fs";
import { patchedFiles, patchEdits, placeHunks } from "./apply-patch.ts";
import { MAX_FACT_FILE_BYTES } from "../facts/lines.ts";
import { markInvoker } from "./invoker.ts";
import { deliverLate, emitPostToolUse } from "./late.ts";
import { editFlow } from "./edit-flow.ts";
import { sessionKey } from "./session-key.ts";
import { createLogger } from "../log.ts";
import { timed, type Timing } from "./timings.ts";

const log = createLogger("carrick-hook");

/** The files the index has rows for. */
const CHECKED = /\.(ts|tsx|mts|cts|graphql|gql)$/;
/** JavaScript: fact lines only (`FACT_FILE`); the probe and re-check flow never see it. */
const FACT_ONLY = /\.(js|jsx|mjs|cjs)$/;

type Payload = {
  cwd?: string;
  session_id?: string;
  agent_id?: string;
  tool_name?: string;
  tool_input?: {
    file_path?: string;
    filePath?: string;
    edits?: Array<{ file_path?: string; old_string?: string; new_string?: string } | null>;
    /** Claude Code's `Edit`: the text the edit replaced, and the text it put in. */
    old_string?: string;
    new_string?: string;
    /** Codex's `apply_patch`: the patch text itself. */
    command?: unknown;
  };
  /** Claude Code's `Edit` and `MultiEdit` result: the hunks it applied; a `Write`'s also says whether it created the file. */
  tool_response?: {
    structuredPatch?: Array<{ newStart?: unknown; lines?: unknown }>;
    type?: unknown;
  };
};

/**
 * Every file one tool call left on disk, whichever host made it.
 *
 * A named file wins where there is one: that is Claude Code, and its path is
 * absolute. Otherwise the `command` field is read as a patch, which is Codex's
 * `apply_patch` — its paths are relative to the cwd the tool ran in, and a
 * string that is not a patch gives nothing, so nothing here has to ask which
 * host is calling.
 *
 * A shell command that is not a patch reaches the same branch and yields an
 * empty list, which is why this does not test `tool_name`: the patch text
 * recognises itself, and an `apply_patch` run through a shell is recorded on
 * the same terms as one run as a tool.
 */
export function filesFromPayload(payload: Payload): string[] {
  const input = payload.tool_input ?? {};
  const named = input.file_path ?? input.filePath ?? input.edits?.[0]?.file_path ?? null;
  if (named) return [named];
  return typeof input.command === "string" ? patchedFiles(input.command) : [];
}

async function main(timing: Timing): Promise<void> {
  markInvoker("post-edit");
  // `off` stops the work as well as the printing. `lsp` stops only the
  // printing: it decides which surface delivers a verdict, and the session
  // record below (the edit stamp) is not a verdict.
  const channel = resolveChannel({ hooksInstalled: true });
  if (channel.channel === "off") {
    log("CARRICK_CHANNEL=off; doing nothing");
    return;
  }

  let payload: Payload;
  try {
    payload = JSON.parse((await readStdin()) || "{}") as Payload;
  } catch (error) {
    log("unparseable payload", String(error));
    return;
  }

  // A patch can name a file this index has no rows for beside one it does, so
  // the filter is per file rather than a reason to drop the call.
  const cwd = payload.cwd ?? null;
  const absolute = (file: string) => (path.isAbsolute(file) ? file : path.resolve(cwd ?? process.cwd(), file));
  const named = filesFromPayload(payload);
  const edited = named.filter((file) => CHECKED.test(file)).map(absolute);
  const factOnly = named.filter((file) => FACT_ONLY.test(file)).map(absolute);
  const session = sessionKey(payload);
  if (edited.length === 0 && factOnly.length === 0) {
    // Nothing of ours to check in this edit, but a finished background answer
    // is still owed to the model (B3: every carrier delivers).
    if (channel.channel === "hook") {
      const late = deliverLate(session);
      emitPostToolUse(late);
    }
    return;
  }

  timing.file = (edited[0] ?? factOnly[0])!;
  const changes = await changesFromPayload(payload, [...edited, ...factOnly], cwd);
  await editFlow({ cwd, session, channel: channel.channel, timing, changes, factOnly }, edited);
}

type Change = { lines?: number[]; removed?: number[]; snippets?: string[]; whole?: boolean };

/**
 * Whether git, asked in the file's folder, says it does not track `file`;
 * false when git cannot say. Loaded only on this path, which a created file
 * alone takes.
 */
async function untracked(file: string): Promise<boolean> {
  const { trackedPaths } = await import("../init/repo-copies.ts");
  const tracked = trackedPaths(path.dirname(file), [path.basename(file)]);
  return tracked !== null && tracked.size === 0;
}

/** The file's changed lines against HEAD (`facts/diff.ts`), or null when not known. */
async function againstHead(file: string, created: boolean): Promise<Change | null> {
  const { changesAgainstHead } = await import("../facts/diff.ts");
  return changesAgainstHead(file, created);
}

/**
 * Where the edit changed each file, keyed by the absolute path: the lines of
 * the file as it is now that its hunks added and where they removed lines
 * (an `Edit` or `MultiEdit`'s patch, a Codex `apply_patch` hunk placed in the
 * file), else the text it put in, else `whole` for a file this call created
 * that git does not track. A file left out (a `Write` over an existing file,
 * a hunk that cannot be placed and has no diff against HEAD) gets no fact
 * lines (`facts/lines.ts`). An `Edit`'s hunks a formatter has since moved fall
 * back to the diff against HEAD, as a Codex hunk does.
 */
async function changesFromPayload(payload: Payload, edited: string[], cwd: string | null): Promise<Map<string, Change>> {
  const input = payload.tool_input ?? {};
  const named = input.file_path ?? input.filePath ?? input.edits?.[0]?.file_path ?? null;
  if (named === null && typeof input.command === "string") {
    const out = new Map<string, Change>();
    for (const [relative, change] of patchEdits(input.command)) {
      const file = path.isAbsolute(relative) ? relative : path.resolve(cwd ?? process.cwd(), relative);
      if (change === null || !edited.includes(file)) continue;
      if ("added" in change) {
        if (await untracked(file)) out.set(file, { whole: true });
        continue;
      }
      try {
        if (fs.statSync(file).size > MAX_FACT_FILE_BYTES) continue;
        const placed = placeHunks(fs.readFileSync(file, "utf8"), change.hunks);
        // A hunk that cannot be placed: the file's diff against HEAD.
        const found = placed ?? (await againstHead(file, false));
        if (found) out.set(file, found);
      } catch {
        // Unreadable now: nothing said about it.
      }
    }
    return out;
  }
  if (edited.length !== 1) return new Map();
  // A `Write` (its result has a `type`: `create` or `update`) is placed only
  // when it created the file; over an existing file it says nothing, even
  // though its result carries hunks too.
  const kind = payload.tool_response?.type;
  if (payload.tool_name === "Write" || kind === "create" || kind === "update") {
    if (kind === "create" && (await untracked(edited[0]!))) return new Map([[edited[0]!, { whole: true }]]);
    // Over an existing file the call's own hunks are not read (a `Write` says
    // little of what it changed): the file's diff against HEAD is.
    const found = await againstHead(edited[0]!, false);
    return found ? new Map([[edited[0]!, found]]) : new Map();
  }
  const hunks = payload.tool_response?.structuredPatch;
  if (Array.isArray(hunks) && hunks.length > 0) {
    const lines: number[] = [];
    // A removed line is not a line of the file now: it is the gap just above
    // the line that now sits where it was (`facts/match.ts` `gapsOfLines`).
    const removed: number[] = [];
    // Each added line's text must be the file's line at that number now; a
    // formatter that ran after the tool moves them, and then the hunks are unplaced.
    let now: string[];
    try {
      if (fs.statSync(edited[0]!).size > MAX_FACT_FILE_BYTES) return new Map();
      now = fs.readFileSync(edited[0]!, "utf8").split("\n");
    } catch {
      return new Map();
    }
    for (const hunk of hunks) {
      if (typeof hunk?.newStart !== "number" || !Array.isArray(hunk.lines)) return new Map();
      let at = hunk.newStart;
      for (const text of hunk.lines) {
        if (typeof text !== "string" || text.startsWith("\\")) continue;
        if (text.startsWith("+")) {
          if (now[at - 1]?.replace(/\r$/, "") !== text.slice(1).replace(/\r$/, "")) {
            // Unplaced: the file's diff against HEAD, as for a Codex patch.
            const found = await againstHead(edited[0]!, false);
            return found ? new Map([[edited[0]!, found]]) : new Map();
          }
          lines.push(at++);
        } else if (text.startsWith("-")) removed.push(at);
        else at += 1;
      }
    }
    return new Map([[edited[0]!, { lines, removed }]]);
  }
  const texts = [
    changedMiddle(input.old_string, input.new_string),
    ...(Array.isArray(input.edits) ? input.edits : []).map((edit) => changedMiddle(edit?.old_string, edit?.new_string)),
  ].filter((text): text is string => typeof text === "string");
  return texts.length > 0 ? new Map([[edited[0]!, { snippets: texts }]]) : new Map();
}

/**
 * The lines of `after` that differ from `before`: the lines both share at
 * the start and at the end are trimmed, so text the edit only carried along
 * (a neighbouring function) is not read as changed. Null when nothing differs.
 */
function changedMiddle(before: unknown, after: unknown): string | null {
  if (typeof after !== "string") return null;
  const old = typeof before === "string" ? before.split("\n") : [];
  const now = after.split("\n");
  let head = 0;
  while (head < old.length && head < now.length && old[head] === now[head]) head += 1;
  let tail = 0;
  while (tail < old.length - head && tail < now.length - head && old[old.length - 1 - tail] === now[now.length - 1 - tail]) tail += 1;
  const middle = now.slice(head, now.length - tail).join("\n");
  return middle.trim().length > 0 ? middle : null;
}

try {
  await timed("post-edit", main);
} catch (error) {
  // A payload this hook did not expect: the edit stands, nothing is said.
  log("post-edit failed", String(error));
}
process.exit(0);
