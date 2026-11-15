// The post-edit hook runs the probe and the fact lines side by side (A7): the
// wait is the longer of the two, not their sum. Fakes stand in for the three
// calls and record when each started; nothing touches a real fact file.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { editFlow } from "../src/hook/edit-flow.ts";
import { lockPath, queueDir } from "../src/hook/rechecks.ts";
import type { FactOutcome } from "../src/facts/lines.ts";
import type { FactStore } from "../src/facts/store.ts";
import "./helpers.ts";

const DELAY_MS = 200;
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function outcome(present: boolean): FactOutcome {
  return { lines: [], used: [], claims: {}, stale: false, callersRead: [], present, answered: [], ms: DELAY_MS };
}

type Case = {
  subAgent: boolean;
  factFile: boolean;
  /** How long the fake probe takes; default `DELAY_MS`. */
  probeMs?: number;
  /** The fake probe rejects instead of answering. */
  probeRejects?: boolean;
};

async function runCase(t: test.TestContext, c: Case) {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "a7-"));
  const home = path.join(base, "home");
  const root = path.join(base, "ws");
  fs.mkdirSync(home, { recursive: true });
  fs.mkdirSync(path.join(root, ".carrick"), { recursive: true });
  const file = path.join(root, "a.ts");
  const second = path.join(root, "b.ts");
  fs.writeFileSync(file, "export const a = 1;\n");
  fs.writeFileSync(second, "export const b = 1;\n");
  // A live runner holds the lock, so the queue never starts one.
  const lock = lockPath(root);
  fs.mkdirSync(path.dirname(lock), { recursive: true });
  fs.writeFileSync(lock, JSON.stringify({ pid: process.pid, started: new Date().toISOString(), touched: new Date().toISOString() }));
  const priorHome = process.env["HOME"];
  process.env["HOME"] = home;
  t.after(() => {
    if (priorHome === undefined) delete process.env["HOME"];
    else process.env["HOME"] = priorHome;
    fs.rmSync(base, { recursive: true, force: true });
  });

  const started: Record<string, number> = {};
  const probed: string[][] = [];
  const queueDirOf = queueDir(root);
  // How many queue entries exist just before the probe answers.
  let queuedBeforeProbeAnswer: number | null = null;
  const t0 = performance.now();
  const run = {
    openFacts: async () => (c.factFile ? ({ close() {} } as unknown as FactStore) : null),
    factLines: async (_root: string, _edits: unknown, _session: unknown, options: { store?: FactStore | null }) => {
      started["facts"] = performance.now() - t0;
      await sleep(DELAY_MS);
      return outcome(options.store != null);
    },
    probeEdits: async (relatives: string[]) => {
      started["probe"] = performance.now() - t0;
      probed.push(relatives);
      await sleep(c.probeMs ?? DELAY_MS);
      queuedBeforeProbeAnswer = fs.existsSync(queueDirOf) ? fs.readdirSync(queueDirOf).filter((name) => name.endsWith(".json")).length : 0;
      if (c.probeRejects) throw new Error("probe failed to start");
      return { results: relatives.map(() => null), failure: null, ms: DELAY_MS };
    },
  } as unknown as NonNullable<Parameters<typeof editFlow>[0]["run"]>;
  await editFlow(
    { cwd: root, session: "a7-session", subAgent: c.subAgent, channel: "hook", run },
    [file, second],
  );
  const entries = fs.existsSync(queueDirOf)
    ? fs
        .readdirSync(queueDirOf)
        .filter((name) => name.endsWith(".json"))
        .map((name) => JSON.parse(fs.readFileSync(path.join(queueDirOf, name), "utf8")) as { instead_of_probe?: boolean })
    : [];
  return { started, probed, total: performance.now() - t0, entries, queuedBeforeProbeAnswer };
}

test("a sub-agent edit with a fact file probes while the fact lines run", async (t) => {
  const r = await runCase(t, { subAgent: true, factFile: true });
  assert.equal(r.probed.length, 1);
  assert.ok(Math.abs(r.started["probe"]! - r.started["facts"]!) < 50, `probe started ${r.started["probe"]} ms, facts ${r.started["facts"]} ms`);
  assert.ok(r.total < DELAY_MS * 1.5, `waited ${r.total} ms, not the longer of the two`);
});

test("a main-session edit with no fact file probes while the fact lines run", async (t) => {
  const r = await runCase(t, { subAgent: false, factFile: false });
  assert.equal(r.probed.length, 1);
  assert.ok(Math.abs(r.started["probe"]! - r.started["facts"]!) < 50, `probe started ${r.started["probe"]} ms, facts ${r.started["facts"]} ms`);
  assert.ok(r.total < DELAY_MS * 1.5, `waited ${r.total} ms, not the longer of the two`);
});

test("a main-session edit with a fact file probes while the fact lines run and queues its files in place of the probe", async (t) => {
  const r = await runCase(t, { subAgent: false, factFile: true });
  assert.deepEqual(r.probed, [["a.ts", "b.ts"]]);
  assert.ok(Math.abs(r.started["probe"]! - r.started["facts"]!) < 50, `probe started ${r.started["probe"]} ms, facts ${r.started["facts"]} ms`);
  assert.ok(r.total < DELAY_MS * 1.5, `waited ${r.total} ms, not the longer of the two`);
  assert.equal(r.entries.length, 2);
  for (const entry of r.entries) assert.equal(entry.instead_of_probe, true);
});

test("with a fact file, the queue entries exist before the probe answers", async (t) => {
  const r = await runCase(t, { subAgent: false, factFile: true, probeMs: 2 * DELAY_MS });
  assert.equal(r.queuedBeforeProbeAnswer, 2);
});

test("a probe that rejects does not lose the edit: the fact lines print and both files are queued, with or without a fact file", async (t) => {
  const withFacts = await runCase(t, { subAgent: false, factFile: true, probeRejects: true });
  assert.equal(withFacts.entries.length, 2);
  for (const entry of withFacts.entries) assert.equal(entry.instead_of_probe, true);
  const without = await runCase(t, { subAgent: false, factFile: false, probeRejects: true });
  assert.equal(without.entries.length, 2);
});
