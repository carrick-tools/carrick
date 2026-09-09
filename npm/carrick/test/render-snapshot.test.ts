// The agent channel, frozen byte for byte.
//
// The hook context is the only text a coding agent reads (design record
// 2026-09-09, "The audiences and what reaches them"), so a change made for a
// human surface may not move a single byte of it. This snapshots what
// `renderPostToolUse` and `renderSessionStart` produce for every fixture on
// disk and fails on any difference, including whitespace.
//
// It is deliberately blunt: no assertion about the content, one comparison of
// the whole file. Regenerate with CARRICK_UPDATE_SNAPSHOT=1 only when the
// change to the agent's text is the point of the commit, and say so in the
// commit message.
//
// `carrick check`'s own terminal output is written by the Rust CLI and never by
// this package, so nothing here can reach it; the JSON it prints is parsed by
// `contract.ts` and rendered by `render.ts`, which is what this covers.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { parseCheckResult, parseStatusResult } from "../src/contract.ts";
import { renderPostToolUse, renderSessionStart } from "../src/render.ts";
import { fixturePath, testDir } from "./helpers.ts";

const SNAPSHOT = fixturePath("agent-channel.snapshot.txt");
/** The path the hook would print, so the rendered bytes include a display file. */
const DISPLAY_FILE = "user-service/src/routes/users.ts";

function fixtureNames(): string[] {
  return fs
    .readdirSync(path.join(testDir, "fixtures"))
    .filter((name) => name.endsWith(".json"))
    .sort();
}

function render(): string {
  const blocks: string[] = [];
  for (const name of fixtureNames()) {
    const raw = fs.readFileSync(fixturePath(name), "utf8");
    const check = parseCheckResult(raw);
    if (check) {
      blocks.push(`### ${name} :: renderPostToolUse\n${renderPostToolUse(check, DISPLAY_FILE) ?? "(silent)"}`);
      continue;
    }
    const status = parseStatusResult(raw);
    if (status) {
      blocks.push(`### ${name} :: renderSessionStart\n${renderSessionStart(status)}`);
      continue;
    }
    blocks.push(`### ${name} :: neither schema`);
  }
  return `${blocks.join("\n\n")}\n`;
}

test("the agent's text is byte-identical to the snapshot", () => {
  const rendered = render();
  if (process.env["CARRICK_UPDATE_SNAPSHOT"] === "1") {
    fs.writeFileSync(SNAPSHOT, rendered);
  }
  const expected = fs.readFileSync(SNAPSHOT, "utf8");
  assert.equal(
    rendered,
    expected,
    "the hook context changed; regenerate with CARRICK_UPDATE_SNAPSHOT=1 only when that is the point of the change",
  );
});
