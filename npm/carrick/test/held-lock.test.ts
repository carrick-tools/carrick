// The lock helpers shared by the re-check runner and the held-compiler daemon.
// `rechecks.test.ts` covers them through the runner; these pin them on a bare
// path with no workspace around it.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { lockHeld, readJson, releaseLock, STALE_SLACK_MS, takeLock, touchLock, type Lock } from "../src/hook/lock.ts";

function scratch(): { file: string; done: () => void } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "held-lock-"));
  return { file: path.join(dir, "x", "some.lock"), done: () => fs.rmSync(dir, { recursive: true, force: true }) };
}

test("a lock on any path is taken once, and a live holder keeps it", () => {
  const s = scratch();
  try {
    const now = Date.now();
    assert.equal(takeLock(s.file, process.pid, now, 1000), true);
    // The holder is alive and fresh: another pid cannot take it.
    assert.equal(takeLock(s.file, process.ppid, now + 10, 1000), false);
    assert.equal(readJson<Lock>(s.file)?.pid, process.pid);
  } finally {
    s.done();
  }
});

test("a lock nobody touched within the stale limit is replaced", () => {
  const s = scratch();
  try {
    const now = Date.now();
    assert.equal(takeLock(s.file, process.pid, now, 1000), true);
    const later = now + 1000 + STALE_SLACK_MS + 1;
    assert.equal(lockHeld(readJson<Lock>(s.file), later, 1000), false);
    assert.equal(takeLock(s.file, process.ppid, later, 1000), true);
    assert.equal(readJson<Lock>(s.file)?.pid, process.ppid);
  } finally {
    s.done();
  }
});

test("only the holder releases or touches a lock", () => {
  const s = scratch();
  try {
    const now = Date.now();
    takeLock(s.file, process.pid, now, 1000);
    releaseLock(s.file, process.ppid);
    assert.equal(fs.existsSync(s.file), true);
    touchLock(s.file, process.ppid, now + 500);
    assert.equal(readJson<Lock>(s.file)?.touched, new Date(now).toISOString());
    touchLock(s.file, process.pid, now + 500);
    assert.equal(readJson<Lock>(s.file)?.touched, new Date(now + 500).toISOString());
    releaseLock(s.file, process.pid);
    assert.equal(fs.existsSync(s.file), false);
  } finally {
    s.done();
  }
});

test("a lock handed over by the process that started the holder is taken over", () => {
  const s = scratch();
  try {
    const now = Date.now();
    assert.equal(takeLock(s.file, process.ppid, now, 1000), true);
    assert.equal(takeLock(s.file, process.pid, now + 5, 1000), false);
    assert.equal(takeLock(s.file, process.pid, now + 5, 1000, process.ppid), true);
    assert.equal(readJson<Lock>(s.file)?.pid, process.pid);
  } finally {
    s.done();
  }
});
