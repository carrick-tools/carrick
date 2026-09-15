// The one value that says "this machine" to the hosted index
// (carrick-cloud#890).
//
// Every test states its own home directory. The subject is a file in the
// user's home, and a test that reached this one would be handing the developer
// a new id — or reporting on the one they are actually installed with.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {
  INSTALL_ID_HEADER,
  INSTALL_ID_PATTERN,
  ensureInstallId,
  installIdOrNull,
  installIdPath,
  readInstallId,
  removeInstallId,
} from "../src/init/install-id.ts";

/** A home directory with nothing in it, cleaned up when the test ends. */
function home(t: { after: (fn: () => void) => void }): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-install-id-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  return root;
}

test("the id is made once and read back after that", (t) => {
  const root = home(t);
  const file = installIdPath(root);
  assert.equal(file, path.join(root, ".carrick", "install-id"));
  assert.equal(readInstallId(root), null);

  const first = ensureInstallId(root);
  // A UUID v4, which is inside what the server accepts, and nothing about this
  // machine: not the hostname, not the user, not a MAC address.
  assert.match(first, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.match(first, INSTALL_ID_PATTERN);
  assert.equal(first.includes(os.hostname()), false);

  // One line, and the file is the id: a second call is a read.
  assert.equal(fs.readFileSync(file, "utf8"), `${first}\n`);
  assert.equal(ensureInstallId(root), first);
  assert.equal(readInstallId(root), first);
  assert.equal(installIdOrNull(root), first);

  // Two machines are two ids.
  const other = home(t);
  assert.notEqual(ensureInstallId(other), first);
});

test("the file is the user's alone", { skip: process.platform === "win32" ? "POSIX modes" : false }, (t) => {
  const root = home(t);
  ensureInstallId(root);
  const file = installIdPath(root);
  assert.equal(fs.statSync(file).mode & 0o777, 0o600);
  assert.equal(fs.statSync(path.dirname(file)).mode & 0o777, 0o700);

  // And a file left unreadable-as-an-id is replaced rather than reported:
  // nobody edits this one on purpose, and a malformed one would send a header
  // the server rejects.
  fs.writeFileSync(file, "not an id\n", { mode: 0o644 });
  assert.equal(readInstallId(root), null);
  const replaced = ensureInstallId(root);
  assert.match(replaced, INSTALL_ID_PATTERN);
  assert.equal(fs.statSync(file).mode & 0o777, 0o600);
});

test("a home directory that will not take a file costs the header, not the setup", (t) => {
  const root = home(t);
  // A file where the directory has to go: `mkdir` fails, and the caller that
  // may not throw — `connectMcpClients` — gets a null and writes no header.
  fs.writeFileSync(path.join(root, ".carrick"), "");
  assert.throws(() => ensureInstallId(root));
  assert.equal(installIdOrNull(root), null);
  assert.equal(readInstallId(root), null);
});

test("remove takes the id and the directory it was alone in", (t) => {
  const root = home(t);
  assert.equal(removeInstallId(root), false);

  ensureInstallId(root);
  assert.equal(removeInstallId(root), true);
  assert.equal(fs.existsSync(installIdPath(root)), false);
  assert.equal(fs.existsSync(path.join(root, ".carrick")), false);
  // Said once: a second run has nothing to report.
  assert.equal(removeInstallId(root), false);

  // The next install is a new machine, which is the only reset there is.
  const before = ensureInstallId(root);
  removeInstallId(root);
  assert.notEqual(ensureInstallId(root), before);

  // Anything else in the directory keeps it.
  fs.writeFileSync(path.join(root, ".carrick", "something-else"), "");
  assert.equal(removeInstallId(root), true);
  assert.ok(fs.existsSync(path.join(root, ".carrick", "something-else")));
});

test("the header the cloud reads this as", () => {
  assert.equal(INSTALL_ID_HEADER, "X-Carrick-Install-Id");
  // The server's own rule, and the file is checked against it on the way in
  // and on the way out (carrick-cloud#890).
  assert.equal(INSTALL_ID_PATTERN.source, "^[A-Za-z0-9_-]{8,64}$");
  for (const rejected of ["", "short", "a".repeat(65), "has space", "punct!"]) {
    assert.equal(INSTALL_ID_PATTERN.test(rejected), false, rejected);
  }
});
