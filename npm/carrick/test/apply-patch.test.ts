// The files one Codex `apply_patch` call leaves on disk (carrick#1335).

import test from "node:test";
import assert from "node:assert/strict";

import { looksLikePatch, patchedFiles } from "../src/hook/apply-patch.ts";

/** A patch as Codex hands it over: the envelope, headers, and body lines. */
const PATCH = [
  "*** Begin Patch",
  "*** Add File: src/util/slug.ts",
  "+export function slugify(input: string): string {",
  '+  return input.toLowerCase().replace(/\\s+/g, "-");',
  "+}",
  "*** Update File: src/routes/users.ts",
  "@@",
  '-import { thing } from "./thing";',
  '+import { slugify } from "../util/slug";',
  "*** Delete File: src/util/old.ts",
  "*** Update File: src/routes/legacy.ts",
  "*** Move to: src/routes/orders.ts",
  "@@",
  "-const a = 1;",
  "+const a = 2;",
  "*** End Patch",
].join("\n");

test("every file a patch leaves on disk is named once, in order", () => {
  assert.deepEqual(patchedFiles(PATCH), [
    "src/util/slug.ts",
    "src/routes/users.ts",
    // The renamed file is named by its destination, and the source it was
    // renamed from is not on disk to be re-checked.
    "src/routes/orders.ts",
  ]);
  // Stated separately, because these two are the whole reason this is not a
  // line-prefix scan: a deleted file cannot be re-checked, and the file a
  // `Move to` renames away from no longer exists.
  assert.equal(patchedFiles(PATCH).includes("src/util/old.ts"), false, "the deleted file");
  assert.equal(patchedFiles(PATCH).includes("src/routes/legacy.ts"), false, "the renamed-from file");
});

test("a file touched twice in one patch is checked once", () => {
  const twice = [
    "*** Begin Patch",
    "*** Update File: src/a.ts",
    "@@",
    "-const a = 1;",
    "+const a = 2;",
    "*** Update File: src/a.ts",
    "@@",
    "-const b = 1;",
    "+const b = 2;",
    "*** End Patch",
  ].join("\n");
  assert.deepEqual(patchedFiles(twice), ["src/a.ts"]);
});

test("a `Move to` with no update above it still names its destination", () => {
  const moved = ["*** Begin Patch", "*** Move to: src/b.ts", "*** End Patch"].join("\n");
  assert.deepEqual(patchedFiles(moved), ["src/b.ts"]);
});

test("anything that is not a patch names no file at all", () => {
  // The recorder hands over whatever a `command` field held, so a shell command
  // and a body line that reads like a header both have to come back empty.
  assert.equal(looksLikePatch("npm test"), false);
  assert.deepEqual(patchedFiles("npm test"), []);
  assert.deepEqual(patchedFiles('echo "*** Update File: src/a.ts"'), []);
  assert.deepEqual(patchedFiles(""), []);
  // An envelope with a header carrying no path is not a file either.
  assert.deepEqual(patchedFiles("*** Begin Patch\n*** Update File: \n*** End Patch"), []);
});

test("carriage returns do not become part of a path", () => {
  const windows = ["*** Begin Patch", "*** Add File: src/a.ts", "+const a = 1;", "*** End Patch"]
    .join("\r\n");
  assert.deepEqual(patchedFiles(windows), ["src/a.ts"]);
});
