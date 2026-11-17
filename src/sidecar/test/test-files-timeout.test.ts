// carrick#4752: the shard runner kills a test file that never finishes and
// names it, instead of stalling the job silently.
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
// @ts-ignore plain .mjs beside the package, no declaration file
const lib = await import(join(root, "scripts", "test-files.mjs"));

test("a hung file is killed and named, a normal file passes", { timeout: 60_000 }, async () => {
  const dir = mkdtempSync(join(tmpdir(), "test-files-timeout-"));
  const hang = join(dir, "hang.test.mjs");
  const fine = join(dir, "fine.test.mjs");
  writeFileSync(hang, `import {test} from "node:test"; test("never", () => new Promise(() => setInterval(() => {}, 1000)));`);
  writeFileSync(fine, `import {test} from "node:test"; test("ok", () => {});`);
  let printed = "";
  const orig = console.error;
  console.error = (m: string) => { printed += m + "\n"; };
  let failed: string[];
  const t0 = Date.now();
  try {
    failed = await lib.runFiles([hang, fine], { timeoutMs: 3000, cwd: dir });
  } finally {
    console.error = orig;
  }
  assert.deepEqual(failed, [hang]);
  assert.match(printed, /TIMED OUT: .*hang\.test\.mjs/);
  assert.ok(Date.now() - t0 < 30_000, "returned within the limit, not at the test's own end");
});
