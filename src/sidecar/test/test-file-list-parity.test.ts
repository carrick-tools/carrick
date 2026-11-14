// carrick#4310: `npm test` and CI's sidecar shards must run the same files.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
// @ts-ignore plain .mjs beside the package, no declaration file
const lib = await import(join(root, "scripts", "test-files.mjs"));

test("every compiled top-level test file is in exactly one CI shard", () => {
  const onDisk = readdirSync(join(root, "dist", "test"))
    .filter((f) => f.endsWith(".test.js"))
    .map((f) => `dist/test/${f}`)
    .sort();
  const all: string[] = lib.shardFiles("all");
  assert.deepEqual(all, onDisk);
  const shards = ["heavy", "rest-1", "rest-2", "rest-3"].flatMap((s) => lib.shardFiles(s) as string[]);
  assert.equal(new Set(shards).size, shards.length, "a file is in two shards");
  assert.deepEqual([...shards].sort(), onDisk);
});

test("npm test and the CI sidecar job both read scripts/test-files.mjs", () => {
  const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  assert.match(pkg.scripts.test, /scripts\/test-files\.mjs run all/);
  assert.doesNotMatch(pkg.scripts.test, /\*\*/);
  const ci = readFileSync(join(root, "..", "..", ".github", "workflows", "ci.yml"), "utf8");
  assert.match(ci, /node scripts\/test-files\.mjs run "\$\{\{ matrix\.shard \}\}"/);
});
