// The repo selection in `carrick-workspace.json` (carrick#1344).
//
// The file is shared: a user writes their own repo list and their own
// exclusions in it, and `carrick init` adds to the same list. So every test
// here is about the line between the two.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  excludedRepos,
  removeSelection,
  withExclusions,
  withoutOurExclusions,
  writeSelection,
  WORKSPACE_FILE,
} from "../src/init/workspace-file.ts";

function folder(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "carrick-workspace-file-"));
}

test("a selection is written as an exclusion, and recorded as ours", () => {
  const written = withExclusions(null, ["web"]);
  assert.deepEqual(JSON.parse(written.body), { exclude: ["web"], carrick: { exclude: ["web"] } });
  assert.deepEqual(written.added, ["web"]);
  assert.equal(written.changed, true);
});

test("a repo list and an exclusion of somebody else's are kept, and not claimed", () => {
  const theirs = `${JSON.stringify({ repos: ["../shared"], exclude: ["fixtures"], other: 1 }, null, 2)}\n`;
  const written = withExclusions(theirs, ["web", "fixtures"]);
  const document = JSON.parse(written.body);
  // Their keys, in their order, with only the new name added.
  assert.deepEqual(document.repos, ["../shared"]);
  assert.equal(document.other, 1);
  assert.deepEqual(document.exclude, ["fixtures", "web"]);
  // `fixtures` was already excluded, so this run did not exclude it and does
  // not get to take it back.
  assert.deepEqual(written.added, ["web"]);
  assert.deepEqual(document.carrick, { exclude: ["web"] });
});

test("a second run over the same selection changes nothing", () => {
  const first = withExclusions(null, ["web"]);
  const second = withExclusions(first.body, ["web"]);
  assert.equal(second.changed, false);
  assert.deepEqual(second.added, []);
});

test("remove takes back what init excluded and leaves what it did not", () => {
  const written = withExclusions(
    `${JSON.stringify({ repos: ["../shared"], exclude: ["fixtures"] }, null, 2)}\n`,
    ["web"],
  );
  const cleaned = withoutOurExclusions(written.body);
  assert.deepEqual(cleaned.removed, ["web"]);
  const document = JSON.parse(cleaned.body);
  assert.deepEqual(document.exclude, ["fixtures"]);
  assert.deepEqual(document.repos, ["../shared"]);
  assert.equal("carrick" in document, false);
  // Their file, so it stays a file.
  assert.equal(cleaned.empty, false);
});

test("a file that is nothing but the selection is the caller's to delete", () => {
  const cleaned = withoutOurExclusions(withExclusions(null, ["web"]).body);
  assert.equal(cleaned.empty, true);
  assert.deepEqual(JSON.parse(cleaned.body), {});
});

test("a file with no selection of ours in it is not rewritten", () => {
  const theirs = `${JSON.stringify({ exclude: ["fixtures"] }, null, 2)}\n`;
  const cleaned = withoutOurExclusions(theirs);
  assert.deepEqual(cleaned.removed, []);
  assert.equal(cleaned.changed, false);
});

test("a workspace file that is not JSON is reported, never overwritten", () => {
  assert.throws(() => withExclusions("{ not json", ["web"]));
  assert.throws(() => withoutOurExclusions("[]"), /not a JSON object/);
});

test("the round trip on disk leaves the folder as it was found", () => {
  const workspace = folder();
  try {
    assert.equal(writeSelection(workspace, []), null, "nothing excluded, nothing written");
    assert.equal(fs.existsSync(path.join(workspace, WORKSPACE_FILE)), false);

    writeSelection(workspace, ["web"]);
    assert.deepEqual(excludedRepos(workspace), ["web"]);

    const removed = removeSelection(workspace);
    assert.deepEqual(removed?.removed, ["web"]);
    assert.equal(removed?.deleted, true);
    assert.equal(fs.existsSync(path.join(workspace, WORKSPACE_FILE)), false);
    assert.equal(removeSelection(workspace), null, "and there is nothing left to take back");
  } finally {
    fs.rmSync(workspace, { recursive: true, force: true });
  }
});

test("a user's own file survives the removal, with their entries in it", () => {
  const workspace = folder();
  try {
    fs.writeFileSync(
      path.join(workspace, WORKSPACE_FILE),
      `${JSON.stringify({ repos: ["../shared"] }, null, 2)}\n`,
    );
    writeSelection(workspace, ["web"]);
    const removed = removeSelection(workspace);
    assert.equal(removed?.deleted, false);
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(workspace, WORKSPACE_FILE), "utf8")), {
      repos: ["../shared"],
    });
  } finally {
    fs.rmSync(workspace, { recursive: true, force: true });
  }
});
