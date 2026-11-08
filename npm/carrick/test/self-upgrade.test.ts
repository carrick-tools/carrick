// An existing global install upgrades itself at session start (carrick#1232;
// docs/reference/update-check.md).
//
// The claims under test: a global install that already exists is upgraded in a
// detached child started once per release; a project dependency, an npx run, a
// suppressed machine, CI and a major version jump get only the notice; the child
// never installs onto a machine with no global, never installs a release that is
// not newer, and never fails; and the session after an upgrade says so once.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { readOutcome, runUpgrade, writeOutcome } from "../src/upgrade.ts";
import type { UpgradeRecord } from "../src/upgrade.ts";
import { upgradeLines } from "../src/hook/upgrade.ts";
import { writeUpdateState } from "../src/update.ts";
import type { GlobalMachine } from "../src/global-install.ts";
import type { InstallShape } from "../src/update.ts";

function temporary(prefix: string): string {
  return fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
}

/** A machine's environment: its own config directory, no switch turned on. */
function machineEnv(extra: NodeJS.ProcessEnv = {}): NodeJS.ProcessEnv {
  return { XDG_CONFIG_HOME: temporary("carrick-s5-config-"), ...extra };
}

/** Where a global npm install of this package sits: no manifest above it. */
function globalRoot(): string {
  return path.join(temporary("carrick-s5-global-"), "lib", "node_modules", "carrick");
}

/** Where a project's dependency sits: the project's manifest names carrick. */
function projectRoot(): string {
  const project = temporary("carrick-s5-project-");
  fs.writeFileSync(
    path.join(project, "package.json"),
    JSON.stringify({ name: "app", devDependencies: { carrick: "0.3.148" } }),
  );
  return path.join(project, "node_modules", "carrick");
}

function npxRoot(): string {
  return path.join(temporary("carrick-s5-npx-"), "_npx", "abc123", "node_modules", "carrick");
}

type Start = { command: string; args: string[] };

function session(options: {
  env: NodeJS.ProcessEnv;
  root: string;
  current?: string;
  latest?: string | null;
  now?: number;
  starts: Start[];
  throwOnStart?: boolean;
}): string[] {
  if (options.latest !== undefined) {
    writeUpdateState({ checked_at: new Date(options.now ?? Date.now()).toISOString(), latest: options.latest }, options.env);
  }
  return upgradeLines({
    env: options.env,
    root: options.root,
    current: options.current ?? "0.3.148",
    now: options.now ?? Date.now(),
    start: (command, args) => {
      if (options.throwOnStart) throw new Error("spawn failed");
      options.starts.push({ command, args });
    },
  });
}

const L1A =
  "carrick 0.3.148 is installed and 0.3.149 is published. Updating it in the background with `npm install -g carrick@0.3.149`; the next session runs 0.3.149.";
const HOUR = 60 * 60_000;

test("an older npm global with a newer cached latest starts the child once and says so", () => {
  const env = machineEnv();
  const root = globalRoot();
  const starts: Start[] = [];
  const now = Date.now();
  const first = session({ env, root, latest: "0.3.149", now, starts });
  assert.deepEqual(first, [L1A]);
  assert.equal(starts.length, 1);
  assert.match(starts[0]!.args.join(" "), /self-upgrade\.js$/);
  const second = session({ env, root, now: now + HOUR, starts });
  assert.equal(starts.length, 1, "a second session inside the TTL starts nothing");
  assert.equal(second.length, 1, "and still carries today's notice");
  assert.match(second[0]!, /is installed and 0\.3\.149 is published\. Update with/);
});

test("a project dependency starts nothing and prints today's notice", () => {
  const env = machineEnv();
  const starts: Start[] = [];
  const lines = session({ env, root: projectRoot(), latest: "0.3.149", starts });
  assert.equal(starts.length, 0);
  assert.equal(lines.length, 1);
  assert.match(lines[0]!, /is installed \(pinned in .*package\.json\) and 0\.3\.149 is published\. Update with `npm install -D carrick@latest`/);
});

test("an npx run starts nothing and prints today's notice", () => {
  const starts: Start[] = [];
  const lines = session({ env: machineEnv(), root: npxRoot(), latest: "0.3.149", starts });
  assert.equal(starts.length, 0);
  assert.match(lines[0]!, /Update with `npx --yes carrick@latest`/);
});

test("an upgraded outcome prints once, and the next session says nothing about it", () => {
  const env = machineEnv();
  const root = globalRoot();
  const starts: Start[] = [];
  assert.ok(
    writeOutcome(
      { kind: "upgraded", from: "0.3.148", to: "0.3.149", warning: null, at: new Date().toISOString() },
      env,
    ),
  );
  const first = session({ env, root, current: "0.3.149", latest: "0.3.149", starts });
  assert.deepEqual(first, ["Carrick updated itself from 0.3.148 to 0.3.149."]);
  const next = session({ env, root, current: "0.3.149", latest: "0.3.149", starts });
  assert.deepEqual(next, []);
  assert.equal(starts.length, 0);
});

test("a refused outcome prints its warning once and that release is not tried again", () => {
  const env = machineEnv();
  const root = globalRoot();
  const starts: Start[] = [];
  const warning =
    "The global carrick is still 0.3.148: it refused. Run `npm install -g carrick@0.3.149` when that is sorted.";
  writeOutcome(
    { kind: "refused", from: "0.3.148", to: "0.3.149", warning, at: new Date().toISOString() },
    env,
  );
  const now = Date.now();
  const first = session({ env, root, latest: "0.3.149", now, starts });
  assert.equal(first[0], warning);
  assert.equal(starts.length, 0);
  const later = session({ env, root, latest: "0.3.149", now: now + 5 * HOUR, starts });
  assert.equal(starts.length, 0, "past the TTL, the same release starts nothing");
  assert.ok(!later.includes(warning), "and the warning is not repeated");
  const nextRelease = session({
    env,
    root,
    latest: "0.3.150",
    now: now + 10 * HOUR,
    starts,
  });
  assert.equal(starts.length, 1, "the next release is tried afresh");
  assert.match(nextRelease[0]!, /npm install -g carrick@0\.3\.150/);
});

test("CARRICK_NO_UPDATE_CHECK=1 starts nothing and prints nothing", () => {
  const starts: Start[] = [];
  const lines = session({
    env: machineEnv({ CARRICK_NO_UPDATE_CHECK: "1" }),
    root: globalRoot(),
    latest: "0.3.149",
    starts,
  });
  assert.equal(starts.length, 0);
  assert.deepEqual(lines, []);
});

test("CI starts nothing", () => {
  const starts: Start[] = [];
  session({ env: machineEnv({ CI: "true" }), root: globalRoot(), latest: "0.3.149", starts });
  assert.equal(starts.length, 0);
});

test("a new major version starts nothing and prints today's notice", () => {
  const starts: Start[] = [];
  const lines = session({ env: machineEnv(), root: globalRoot(), latest: "1.0.0", starts });
  assert.equal(starts.length, 0);
  assert.match(lines[0]!, /1\.0\.0 is published\. Update with/);
});

test("a latest that is not newer starts nothing", () => {
  const starts: Start[] = [];
  const lines = session({ env: machineEnv(), root: globalRoot(), latest: "0.3.148", starts });
  assert.equal(starts.length, 0);
  assert.deepEqual(lines, []);
});

test("a child that cannot start, or an unreadable outcome, fails open", () => {
  const env = machineEnv();
  const root = globalRoot();
  const starts: Start[] = [];
  fs.mkdirSync(path.join(env["XDG_CONFIG_HOME"]!, "carrick"), { recursive: true });
  fs.writeFileSync(path.join(env["XDG_CONFIG_HOME"]!, "carrick", "self-upgrade.json"), "{not json");
  const lines = session({ env, root, latest: "0.3.149", starts, throwOnStart: true });
  assert.equal(lines.some((line) => line.includes("Updating it in the background")), false);
  assert.equal(starts.length, 0);
});

// The child.

function machineWith(options: {
  versions: Array<string | null | undefined>;
  kind?: InstallShape["kind"];
  ok?: boolean;
}): { machine: GlobalMachine; installs: string[][] } {
  const state = { finds: 0 };
  const installs: string[][] = [];
  const machine: GlobalMachine = {
    find: () => {
      const version = options.versions[state.finds];
      state.finds += 1;
      if (version === undefined) return null;
      return {
        binary: "/usr/local/bin/carrick",
        real: "/usr/local/lib/node_modules/carrick/bin/carrick.mjs",
        version,
      };
    },
    shape: () => ({ kind: options.kind ?? "npm", command: "npm install -g carrick@latest" }),
    install: (argv) => {
      installs.push(argv);
      return options.ok === false ? { ok: false, reason: "it refused" } : { ok: true, reason: null };
    },
  };
  return { machine, installs };
}

test("the child upgrades an existing global to a fresh registry answer and records it", async () => {
  const env = machineEnv();
  const { machine, installs } = machineWith({ versions: ["0.3.148", "0.3.149"] });
  const record = await runUpgrade({ env, machine, current: "0.3.148", latest: async () => "0.3.149" });
  assert.deepEqual(installs, [["npm", "install", "-g", "carrick@0.3.149"]]);
  assert.equal(record?.kind, "upgraded");
  const stored = readOutcome(env);
  assert.equal(stored?.kind, "upgraded");
  assert.equal(stored?.from, "0.3.148");
  assert.equal(stored?.to, "0.3.149");
  assert.equal(stored?.warning, null);
});

test("the child records a refusal with the existing warning", async () => {
  const env = machineEnv();
  const { machine } = machineWith({ versions: ["0.3.148"], ok: false });
  await runUpgrade({ env, machine, current: "0.3.148", latest: async () => "0.3.149" });
  const stored = readOutcome(env) as UpgradeRecord;
  assert.equal(stored.kind, "refused");
  assert.equal(
    stored.warning,
    "The global carrick is still 0.3.148: it refused. Run `npm install -g carrick@0.3.149` when that is sorted.",
  );
});

test("a fresh registry answer older than or equal to the running version installs nothing", async () => {
  for (const answer of ["0.3.148", "0.3.147", null]) {
    const { machine, installs } = machineWith({ versions: ["0.3.148"] });
    await runUpgrade({ env: machineEnv(), machine, current: "0.3.148", latest: async () => answer });
    assert.equal(installs.length, 0, `answer ${answer}`);
  }
});

test("the child never installs onto a machine with no global carrick", async () => {
  const env = machineEnv();
  const { machine, installs } = machineWith({ versions: [] });
  await runUpgrade({ env, machine, current: "0.3.148", latest: async () => "0.3.149" });
  assert.equal(installs.length, 0);
  assert.notEqual(readOutcome(env)?.kind, "upgraded");
});

test("the child never upgrades a project-local carrick that answers on PATH", async () => {
  const { machine, installs } = machineWith({ versions: ["0.3.148"], kind: "project" });
  await runUpgrade({ env: machineEnv(), machine, current: "0.3.148", latest: async () => "0.3.149" });
  assert.equal(installs.length, 0);
});

test("the child never crosses a major version", async () => {
  const { machine, installs } = machineWith({ versions: ["0.3.148"] });
  await runUpgrade({ env: machineEnv(), machine, current: "0.3.148", latest: async () => "1.0.0" });
  assert.equal(installs.length, 0);
});

test("the child does nothing when updates are switched off or in CI", async () => {
  for (const extra of [{ CARRICK_NO_UPDATE_CHECK: "1" }, { CI: "true" }]) {
    const { machine, installs } = machineWith({ versions: ["0.3.148", "0.3.149"] });
    await runUpgrade({ env: machineEnv(extra), machine, current: "0.3.148", latest: async () => "0.3.149" });
    assert.equal(installs.length, 0);
  }
});

test("a throwing registry read or machine fails open", async () => {
  const { machine } = machineWith({ versions: ["0.3.148"] });
  const record = await runUpgrade({
    env: machineEnv(),
    machine: {
      ...machine,
      find: () => {
        throw new Error("boom");
      },
    },
    current: "0.3.148",
    latest: async () => "0.3.149",
  });
  assert.equal(record, null);
});
