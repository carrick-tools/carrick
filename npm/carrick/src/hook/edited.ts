// What one edited file is, shared by the post-edit hook and the background
// re-check runner: whether the scan reads it, which checkout it sits in, and
// which functions a re-check says it newly declares.
//
// Split out of `post-edit.ts` because that file runs `main` on import, so
// nothing else could import from it.

import fs from "node:fs";
import path from "node:path";
import type { CheckResult } from "../contract.ts";
import type { NewFunction } from "./reuse.ts";

/**
 * The files `carrick check --changed` judges: the scanner's own source
 * extensions (`SOURCE_EXTENSIONS` in `src/file_finder.rs`, carrick#904). An API
 * spec file is not counted: the hook cannot tell a spec the scan reads from any
 * other `.json` or `.yaml` without reading the repo, and a config file that
 * moves no contract is what the line used to be wrong about.
 */
const CHECKED_EXTENSIONS = new Set([".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"]);

/** `TEST_FILE_INFIXES` in `src/file_finder.rs`: a test or story file name ends its stem in one. */
const TEST_FILE_INFIXES = [".test", ".spec", ".stories"];

/** `TEST_DIR_NAMES` in `src/file_finder.rs` (plus `.storybook`): trees the scan does not read. */
const TEST_DIR_NAMES = new Set([
  "__tests__",
  "__mocks__",
  "__fixtures__",
  "tests",
  "test",
  "fixtures",
  "e2e",
  ".storybook",
]);

/** Whether `carrick check --changed` would judge this edited file: source, and not a test or story. */
export function isCheckedSource(file: string): boolean {
  const extension = path.extname(file).toLowerCase();
  if (!CHECKED_EXTENSIONS.has(extension)) return false;
  const stem = path.basename(file, extension).toLowerCase();
  if (TEST_FILE_INFIXES.some((infix) => stem.endsWith(infix))) return false;
  // Directory names above the file. The scanner reads them relative to the
  // scan root; the hook does not know it, so a checkout under a directory
  // named `test` hides its source here: the line is then missing, not wrong.
  const directories = path.dirname(file).split(/[\\/]/);
  return !directories.some((name) => TEST_DIR_NAMES.has(name.toLowerCase()));
}

/**
 * What the re-check said this file declares and the index does not
 * (carrick#1330).
 *
 * Nothing is printed about it here. This hook fires on every edit and most
 * edits add no function; the one line about the ones that did belongs at the
 * end of the task, where it costs one model turn instead of one per edit. All
 * this does is put the names somewhere the Stop hook can find them.
 */
export function newFunctions(result: CheckResult, file: string): NewFunction[] {
  // Test, spec and story files never warrant a reuse check (carrick#2936).
  if (!isCheckedSource(file)) return [];
  return (result.recheck?.new_functions ?? []).map((entry) => ({
    name: entry.name,
    // The path this hook computed, not the one the binary echoes: it is the
    // checkout-relative path the index holds, whatever root the edit ran from.
    file,
    indexCommit: result.index_commit ?? "",
  }));
}

/**
 * `file` relative to the nearest checkout top at or below `root`: the nearest
 * directory holding `.git` (a folder, or the file a linked worktree has). An
 * edit inside a nested worktree is then keyed as the index keys it, not with
 * the worktree's own prefix (carrick#2936). With no nested checkout it is
 * relative to `root`, as before.
 */
export function checkoutRelative(root: string, file: string): string {
  let dir = path.dirname(file);
  while (dir !== root && path.relative(root, dir) && !path.relative(root, dir).startsWith("..")) {
    if (fs.existsSync(path.join(dir, ".git"))) return path.relative(dir, file);
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  return path.relative(root, file);
}
