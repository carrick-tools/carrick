// Ratchet (carrick#5076): no agent-facing text tells the agent to run
// `carrick check --changed`, to wait for it, or to run it before it finishes.
// Each edit is re-checked in the background and the answer rides a later hook line.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const ROOTS = [path.join(here, "..", "src"), path.join(here, "..", "templates")];
const BANNED = /[Rr]un `carrick check --changed`|`carrick check --changed` lists them|before you (finish|say it is done)/;

function files(dir: string): string[] {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = path.join(dir, entry.name);
    return entry.isDirectory() ? files(full) : [full];
  });
}

test("no source or template file tells an agent to run the whole-change check", () => {
  const hits = ROOTS.flatMap(files).flatMap((file) =>
    fs
      .readFileSync(file, "utf8")
      .split("\n")
      .flatMap((line, index) => (BANNED.test(line) ? [`${path.relative(here, file)}:${index + 1}: ${line.trim()}`] : [])),
  );
  assert.deepEqual(hits, []);
});
