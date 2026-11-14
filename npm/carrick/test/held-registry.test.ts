// The machine-wide registry of held compilers (D3): what is running, how much
// memory it holds, and whether one more fits.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {
  ADMISSION_MB,
  admits,
  liveEntries,
  memoryBudgetMb,
  registryDir,
  removeEntry,
  writeEntry,
  type EngineEntry,
} from "../src/held/registry.ts";

function scratch(): { home: string; done: () => void } {
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "h"));
  return { home, done: () => fs.rmSync(home, { recursive: true, force: true }) };
}

const entry = (over: Partial<EngineEntry>): EngineEntry => ({
  pid: process.pid,
  root: "/r",
  service: "api",
  rss_mb: 2000,
  last_used: 1,
  ...over,
});

test("an entry is written, listed and removed", () => {
  const s = scratch();
  try {
    writeEntry(s.home, entry({}));
    writeEntry(s.home, entry({ service: "web", rss_mb: 500 }));
    assert.deepEqual(
      liveEntries(s.home).map((e) => `${e.service}:${e.rss_mb}`).sort(),
      ["api:2000", "web:500"],
    );
    // Writing the same engine again replaces it.
    writeEntry(s.home, entry({ rss_mb: 2100 }));
    assert.equal(liveEntries(s.home).filter((e) => e.service === "api")[0]!.rss_mb, 2100);
    removeEntry(s.home, process.pid, "/r", "api");
    assert.deepEqual(liveEntries(s.home).map((e) => e.service), ["web"]);
  } finally {
    s.done();
  }
});

test("an entry whose owner is dead is dropped and its file deleted", () => {
  const s = scratch();
  try {
    // A pid no process holds: spawn-free, take one far above the kernel's range.
    writeEntry(s.home, entry({ pid: 2 ** 22 + 12345 }));
    assert.deepEqual(liveEntries(s.home), []);
    assert.deepEqual(fs.readdirSync(registryDir(s.home)), []);
  } finally {
    s.done();
  }
});

test("a new engine fits when live memory plus 2048 stays within the budget", () => {
  assert.equal(ADMISSION_MB, 2048);
  assert.equal(admits([], 4096), true);
  assert.equal(admits([entry({ rss_mb: 2048 })], 4096), true);
  assert.equal(admits([entry({ rss_mb: 2049 })], 4096), false);
  assert.equal(admits([entry({ rss_mb: 1000 }), entry({ service: "b", rss_mb: 1100 })], 4096), false);
});

test("the budget is CARRICK_HELD_MEMORY_MB, 4096 when unset or unusable", () => {
  assert.equal(memoryBudgetMb({}), 4096);
  assert.equal(memoryBudgetMb({ CARRICK_HELD_MEMORY_MB: "8192" }), 8192);
  assert.equal(memoryBudgetMb({ CARRICK_HELD_MEMORY_MB: "lots" }), 4096);
  assert.equal(memoryBudgetMb({ CARRICK_HELD_MEMORY_MB: "-5" }), 4096);
});
