// The allow rule that lets a headless `claude -p` call Carrick's tools rides
// with the hooks (carrick#4881): written where they are, taken out with them,
// and never a user's own entry.

import assert from "node:assert/strict";
import test from "node:test";

import { CARRICK_ALLOW, mergeCarrickHooks, removeCarrickHooks } from "../src/init/settings.ts";

const USER_FILE = `${JSON.stringify(
  { model: "x", permissions: { allow: ["Read", "Bash(npm test)"], deny: ["Bash(rm *)"] }, hooks: { Stop: [{ hooks: [{ type: "command", command: "echo mine" }] }] } },
  null,
  2,
)}\n`;

test("merge keeps a user's allow entries and other keys, adds ours once, and a second merge changes nothing", () => {
  const first = mergeCarrickHooks(USER_FILE);
  const doc = JSON.parse(first.body);
  assert.deepEqual(doc.permissions.allow, ["Read", "Bash(npm test)", CARRICK_ALLOW]);
  assert.deepEqual(doc.permissions.deny, ["Bash(rm *)"]);
  assert.equal(doc.model, "x");
  const second = mergeCarrickHooks(first.body);
  assert.equal(second.changed, false);
  assert.equal(second.body, first.body);
});

test("a file with no permissions gains them, and remove gives the pre-init bytes back", () => {
  const before = `${JSON.stringify({ model: "x", hooks: { Stop: [{ hooks: [{ type: "command", command: "echo mine" }] }] } }, null, 2)}\n`;
  const merged = mergeCarrickHooks(before);
  assert.deepEqual(JSON.parse(merged.body).permissions, { allow: [CARRICK_ALLOW] });
  assert.equal(removeCarrickHooks(merged.body).body, before);
  // A user's own entries survive the removal.
  assert.equal(removeCarrickHooks(mergeCarrickHooks(USER_FILE).body).body, USER_FILE);
});

test("a file holding the allow entry and none of our hooks is the user's and stays byte for byte", () => {
  const theirs = `${JSON.stringify({ permissions: { allow: [CARRICK_ALLOW] } }, null, 4)}\n`;
  const removed = removeCarrickHooks(theirs);
  assert.equal(removed.changed, false);
  assert.equal(removed.body, theirs);
});
