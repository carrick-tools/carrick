// The Stop hook's check line (carrick#2933): edits after the last
// `carrick check` earn one line asking for `carrick check --changed`, once per
// edit set, read from the session's transcript.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fakeEnv, fixturePath, runHook } from "./helpers.ts";

function tempDir(t: { after: (fn: () => void) => void }, prefix: string): string {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function transcript(name: string): string {
  return fixturePath(path.join("transcripts", name));
}

async function stop(
  home: string,
  payload: Record<string, unknown>,
): Promise<{ code: number | null; context: string | null; stdout: string }> {
  const run = await runHook("stop.ts", {
    payload: { hook_event_name: "Stop", ...payload },
    env: fakeEnv({ HOME: home }),
  });
  const context =
    run.stdout === ""
      ? null
      : (JSON.parse(run.stdout) as { hookSpecificOutput: { additionalContext: string } })
          .hookSpecificOutput.additionalContext;
  return { code: run.code, context, stdout: run.stdout };
}

test("edits with no check earn the check line, with the count of files", async (t) => {
  const home = tempDir(t, "carrick-home-");
  const run = await stop(home, {
    session_id: "chk-1",
    transcript_path: transcript("edit-no-check.jsonl"),
  });
  assert.equal(run.code, 0);
  assert.ok(run.context, "expected the check line");
  // Two source files (one edited twice); the .yaml edit is not judged by --changed.
  assert.match(run.context, /This task edited 2 files and has not run `carrick check`\./);
  // Ends on the same ask as the post-edit "not re-checked" line (render.ts).
  assert.match(
    run.context,
    /The per-edit re-check covers only the file each edit touched, so run `carrick check --changed` before you finish \(it can take a few minutes; allow up to five\)\.$/,
  );
  // Same channel as the reuse line: never a block.
  const parsed = JSON.parse(run.stdout) as Record<string, unknown>;
  assert.equal(parsed["decision"], undefined);
});

test("edits followed by a check say nothing", async (t) => {
  const home = tempDir(t, "carrick-home-");
  const run = await stop(home, {
    session_id: "chk-2",
    transcript_path: transcript("edit-then-check.jsonl"),
  });
  assert.equal(run.code, 0);
  assert.equal(run.stdout, "");
});

test("an edit after a check earns the line, counting only the later edits", async (t) => {
  const home = tempDir(t, "carrick-home-");
  const run = await stop(home, {
    session_id: "chk-3",
    transcript_path: transcript("check-then-edit.jsonl"),
  });
  assert.equal(run.code, 0);
  assert.ok(run.context, "expected the check line");
  // The edit before the check is not counted; a MultiEdit that names its file
  // inside `edits` (the shape post-edit.ts also reads) is.
  assert.match(run.context, /This task edited 1 file since its last `carrick check`\./);
});

test("the line is said once per edit set, and again after a newer edit", async (t) => {
  const home = tempDir(t, "carrick-home-");
  const dir = tempDir(t, "carrick-transcript-");
  const file = path.join(dir, "session.jsonl");
  fs.copyFileSync(transcript("edit-no-check.jsonl"), file);

  const first = await stop(home, { session_id: "chk-4", transcript_path: file });
  assert.ok(first.context, "expected the check line on the first stop");

  const second = await stop(home, { session_id: "chk-4", transcript_path: file });
  assert.equal(second.code, 0);
  assert.equal(second.stdout, "", "the same edit set is not named twice");

  fs.appendFileSync(
    file,
    `${JSON.stringify({
      type: "assistant",
      message: {
        role: "assistant",
        content: [
          { type: "tool_use", id: "toolu_99", name: "Edit", input: { file_path: "/w/src/late.ts" } },
        ],
      },
    })}\n`,
  );
  const third = await stop(home, { session_id: "chk-4", transcript_path: file });
  assert.ok(third.context, "a newer edit earns the line again");
  assert.match(third.context, /This task edited 3 files and has not run `carrick check`\./);
});

test("a stop the hook itself caused is silent", async (t) => {
  const home = tempDir(t, "carrick-home-");
  const run = await stop(home, {
    session_id: "chk-5",
    stop_hook_active: true,
    transcript_path: transcript("edit-no-check.jsonl"),
  });
  assert.equal(run.code, 0);
  assert.equal(run.stdout, "");
});

test("markdown and text edits are not source edits", async (t) => {
  const home = tempDir(t, "carrick-home-");
  const run = await stop(home, {
    session_id: "chk-6",
    transcript_path: transcript("markdown-only.jsonl"),
  });
  assert.equal(run.code, 0);
  assert.equal(run.stdout, "");
});

test("no transcript, or one that cannot be read, is silent and exits 0", async (t) => {
  const home = tempDir(t, "carrick-home-");
  const dir = tempDir(t, "carrick-transcript-");
  const garbled = path.join(dir, "garbled.jsonl");
  fs.writeFileSync(garbled, "{not json\n\u0000\u0001\n");
  for (const transcript_path of [undefined, path.join(dir, "missing.jsonl"), dir, garbled, 42]) {
    const run = await stop(home, { session_id: "chk-7", transcript_path });
    assert.equal(run.code, 0, String(transcript_path));
    assert.equal(run.stdout, "", String(transcript_path));
  }
});

test("the channel off silences the check line too", async (t) => {
  const home = tempDir(t, "carrick-home-");
  const run = await runHook("stop.ts", {
    payload: {
      hook_event_name: "Stop",
      session_id: "chk-8",
      transcript_path: transcript("edit-no-check.jsonl"),
    },
    env: fakeEnv({ HOME: home, CARRICK_CHANNEL: "off" }),
  });
  assert.equal(run.code, 0);
  assert.equal(run.stdout, "");
});

test("config, spec-data, test and story edits are not what --changed judges", async (t) => {
  const home = tempDir(t, "carrick-home-");
  const run = await stop(home, {
    session_id: "chk-nc",
    transcript_path: transcript("not-checked-files.jsonl"),
  });
  assert.equal(run.code, 0);
  assert.equal(run.stdout, "");
});

test("the check line says how long the check takes", async (t) => {
  const home = tempDir(t, "carrick-home-");
  const run = await stop(home, {
    session_id: "chk-time",
    transcript_path: transcript("edit-no-check.jsonl"),
  });
  assert.ok(run.context?.includes("it can take a few minutes; allow up to five"), run.context ?? "");
});
