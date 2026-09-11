// Picking up the hosted index a first CI scan wrote, from a session start
// (carrick#955).
//
// Two things this must not do: ask for a re-index when there is nothing to
// wait for, and ask twice for one workspace.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import {
  awaitingHostedIndex,
  claimRefresh,
  markerPath,
  refreshInBackground,
} from "../src/hook/refresh.ts";
import { STATUS_SCHEMA, type StatusResult, type StatusService } from "../src/contract.ts";

function statusWith(
  states: Array<StatusService["hosted_state"]>,
  hostedCheckedAt?: string,
): StatusResult {
  return {
    schema: STATUS_SCHEMA,
    ...(hostedCheckedAt ? { hosted_checked_at: hostedCheckedAt } : {}),
    services: states.map((hosted_state, index) => ({
      service: `service-${index}`,
      repo: "/code/repo",
      index_commit: "abc1234",
      routes: 1,
      calls: 1,
      changed_since_index: 0,
      ...(hosted_state ? { hosted_state } : {}),
    })),
  };
}

function workspace(): { root: string; cleanup: () => void } {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-refresh-"));
  fs.mkdirSync(path.join(root, ".carrick"));
  return { root, cleanup: () => fs.rmSync(root, { recursive: true, force: true }) };
}

test("only a connected repo with no hosted index yet is worth waiting for", () => {
  assert.equal(awaitingHostedIndex(statusWith(["no_index_yet"])), true);
  assert.equal(awaitingHostedIndex(statusWith(["enriched", "no_index_yet"])), true);
  for (const state of ["enriched", "not_connected", "not_signed_in", "version_mismatch"] as const) {
    assert.equal(awaitingHostedIndex(statusWith([state])), false);
  }
  // An older index, written before the field existed, asks for nothing.
  assert.equal(awaitingHostedIndex(statusWith([undefined])), false);
});

test("a workspace waiting for its first CI scan refreshes once, in the background", () => {
  const { root, cleanup } = workspace();
  try {
    const started: Array<{ command: string; args: string[] }> = [];
    const start = (command: string, args: string[]): void => {
      started.push({ command, args });
    };
    const now = Date.parse("2026-09-11T12:00:00Z");
    const line = refreshInBackground(root, statusWith(["no_index_yet"]), {
      now,
      start,
      env: { CARRICK_BIN: "carrick" },
    });
    assert.match(line ?? "", /background/);
    assert.deepEqual(started, [{ command: "carrick", args: ["refresh", "--workspace", root] }]);

    // The next session start, inside the cooldown, asks for nothing.
    assert.equal(
      refreshInBackground(root, statusWith(["no_index_yet"]), {
        now: now + 60_000,
        start,
        env: { CARRICK_BIN: "carrick" },
      }),
      null,
    );
    assert.equal(started.length, 1);

    // A session an hour later asks again, because the scan may have landed since.
    assert.ok(
      refreshInBackground(root, statusWith(["no_index_yet"]), {
        now: now + 61 * 60_000,
        start,
        env: { CARRICK_BIN: "carrick" },
      }),
    );
    assert.equal(started.length, 2);
  } finally {
    cleanup();
  }
});

// The first `carrick index` reads the hosted side as it builds, so a session
// opened straight afterwards has nothing new to ask for.
test("an index that just read the hosted side is not asked to read it again", () => {
  const { root, cleanup } = workspace();
  try {
    let started = 0;
    const now = Date.parse("2026-09-11T12:00:00Z");
    const line = refreshInBackground(
      root,
      statusWith(["no_index_yet"], new Date(now - 60_000).toISOString()),
      { now, start: () => { started += 1; }, env: {} },
    );
    assert.equal(line, null);
    assert.equal(started, 0);
    assert.equal(fs.existsSync(markerPath(root)), false);
  } finally {
    cleanup();
  }
});

test("two sessions opening at once produce one refresh", () => {
  const { root, cleanup } = workspace();
  try {
    const now = Date.parse("2026-09-11T12:00:00Z");
    const cooldown = 60 * 60_000;
    const winners = [
      claimRefresh(markerPath(root), now, cooldown),
      claimRefresh(markerPath(root), now, cooldown),
    ];
    assert.deepEqual(winners, [true, false]);
    assert.ok(fs.existsSync(markerPath(root)));
    assert.deepEqual(
      fs.readdirSync(path.join(root, ".carrick")),
      ["last-hook-refresh"],
      "the claim leaves no temporary file behind",
    );
  } finally {
    cleanup();
  }
});

test("a workspace that cannot be claimed is left alone rather than failing", () => {
  const { root, cleanup } = workspace();
  try {
    // A directory where the marker goes: the claim cannot be taken and the
    // session start must still finish.
    fs.mkdirSync(markerPath(root));
    let started = 0;
    const line = refreshInBackground(root, statusWith(["no_index_yet"]), {
      now: Date.now(),
      start: () => { started += 1; },
      env: {},
    });
    assert.equal(line, null);
    assert.equal(started, 0);
  } finally {
    cleanup();
  }
});

test("the cooldown can be stated for a machine that wants it shorter", () => {
  const { root, cleanup } = workspace();
  try {
    let started = 0;
    const options = {
      start: (): void => { started += 1; },
      env: { CARRICK_REFRESH_COOLDOWN_MS: "0" },
    };
    const now = Date.parse("2026-09-11T12:00:00Z");
    refreshInBackground(root, statusWith(["no_index_yet"]), { ...options, now });
    refreshInBackground(root, statusWith(["no_index_yet"]), { ...options, now: now + 1 });
    assert.equal(started, 2);
  } finally {
    cleanup();
  }
});
