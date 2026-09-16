// The update check: what it says, what it never says, and every way it is
// allowed to fail.
//
// Half of these tests are failure paths on purpose. A version check that blocks
// a scan is worse than no version check at all, so "no network", "a 500", "a
// corrupt cache", "a config directory nobody can write" and "a registry
// answering nonsense" each have their own assertion that the answer is silence
// and nothing else.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  CHECK_TTL_MS,
  ciUpdateNotice,
  currentVersion,
  fetchLatest,
  inCi,
  installShape,
  isNewer,
  isScanInvocation,
  isStale,
  readUpdateState,
  scheduleUpdateCheck,
  suppressed,
  updateNotice,
  updateStatePath,
  writeUpdateState,
} from "../src/update.ts";
import { checkVersion } from "../src/init/doctor.ts";

function sandbox(): { env: NodeJS.ProcessEnv; dispose: () => void } {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-update-"));
  return {
    env: { XDG_CONFIG_HOME: root, HOME: root, APPDATA: root },
    dispose: () => fs.rmSync(root, { recursive: true, force: true }),
  };
}

/** A fetch that answers one canned response, and records that it was asked. */
function stubFetch(response: Partial<Response> & { json?: () => Promise<unknown> }): {
  impl: typeof fetch;
  calls: number;
} {
  const state = { calls: 0 };
  const impl = (async () => {
    state.calls += 1;
    return { ok: true, json: async () => ({}), ...response } as Response;
  }) as unknown as typeof fetch;
  return {
    impl,
    get calls() {
      return state.calls;
    },
  };
}

test("only a plain release that is strictly greater counts as newer", () => {
  assert.equal(isNewer("0.3.73", "0.3.68"), true);
  assert.equal(isNewer("0.4.0", "0.3.99"), true);
  assert.equal(isNewer("1.0.0", "0.99.99"), true);
  assert.equal(isNewer("0.3.68", "0.3.68"), false);
  assert.equal(isNewer("0.3.67", "0.3.68"), false);
  // Neither side may be a prerelease: nobody is nudged onto one, and a build
  // from a checkout that carries an odd version is left alone.
  assert.equal(isNewer("0.4.0-rc.1", "0.3.73"), false);
  assert.equal(isNewer("0.4.0", "0.3.73-rc.1"), false);
  assert.equal(isNewer("latest", "0.3.73"), false);
  assert.equal(isNewer("", "0.3.73"), false);
});

test("the cache fails open on every shape it is not", () => {
  const box = sandbox();
  try {
    assert.equal(readUpdateState(box.env), null, "absent");

    fs.mkdirSync(path.dirname(updateStatePath(box.env)), { recursive: true });
    fs.writeFileSync(updateStatePath(box.env), "{ not json");
    assert.equal(readUpdateState(box.env), null, "corrupt");

    fs.writeFileSync(updateStatePath(box.env), JSON.stringify({ latest: "0.9.0" }));
    assert.equal(readUpdateState(box.env), null, "no timestamp");

    fs.writeFileSync(updateStatePath(box.env), JSON.stringify({ checked_at: "now", latest: 7 }));
    assert.deepEqual(readUpdateState(box.env), { checked_at: "now", latest: null }, "latest of the wrong type");
  } finally {
    box.dispose();
  }
});

test("a write that cannot land says so, and says nothing else", () => {
  const box = sandbox();
  try {
    assert.equal(writeUpdateState({ checked_at: "2026-09-16T00:00:00Z", latest: "0.3.73" }, box.env), true);
    assert.deepEqual(readUpdateState(box.env), { checked_at: "2026-09-16T00:00:00Z", latest: "0.3.73" });
    // No stray temporary file survives a successful write.
    const directory = path.dirname(updateStatePath(box.env));
    assert.deepEqual(
      fs.readdirSync(directory).filter((name) => name.endsWith(".tmp")),
      [],
    );
  } finally {
    box.dispose();
  }

  // A config directory that is a file, not a directory: mkdir fails, the write
  // fails, and the answer is `false` rather than a throw.
  const file = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-update-"));
  const blocked = path.join(file, "blocker");
  fs.writeFileSync(blocked, "not a directory");
  try {
    assert.equal(writeUpdateState({ checked_at: "x", latest: null }, { XDG_CONFIG_HOME: blocked }), false);
  } finally {
    fs.rmSync(file, { recursive: true, force: true });
  }
});

test("staleness: absent, fresh, expired, and a clock that went backwards", () => {
  const now = Date.parse("2026-09-16T12:00:00Z");
  assert.equal(isStale(null, now), true);
  assert.equal(isStale({ checked_at: new Date(now - 60_000).toISOString(), latest: null }, now), false);
  assert.equal(isStale({ checked_at: new Date(now - CHECK_TTL_MS).toISOString(), latest: null }, now), true);
  assert.equal(isStale({ checked_at: new Date(now + 60_000).toISOString(), latest: null }, now), true);
  assert.equal(isStale({ checked_at: "not a date", latest: null }, now), true);
});

test("suppression and CI detection", () => {
  assert.equal(suppressed({ CARRICK_NO_UPDATE_CHECK: "1" }), true);
  assert.equal(suppressed({ CARRICK_NO_UPDATE_CHECK: "yes" }), true);
  assert.equal(suppressed({ CARRICK_NO_UPDATE_CHECK: "0" }), false);
  assert.equal(suppressed({ CARRICK_NO_UPDATE_CHECK: "false" }), false);
  assert.equal(suppressed({ CARRICK_NO_UPDATE_CHECK: "" }), false);
  assert.equal(suppressed({}), false);

  assert.equal(inCi({ CI: "true" }), true);
  assert.equal(inCi({ GITHUB_ACTIONS: "true" }), true);
  assert.equal(inCi({}), false);
  // A repository that happens to hold a variable called CI with some other
  // value is not a build machine.
  assert.equal(inCi({ CI: "" }), false);
});

test("install shape names a command for every way this package gets installed", () => {
  const cases: Array<[string, string, string]> = [
    ["npx", "/Users/dev/.npm/_npx/7f3/node_modules/carrick", "npx --yes carrick@latest"],
    ["volta", "/Users/dev/.volta/tools/image/packages/carrick/lib/node_modules/carrick", "volta install carrick@latest"],
    ["bun", "/Users/dev/.bun/install/global/node_modules/carrick", "bun add -g carrick@latest"],
    ["pnpm", "/Users/dev/Library/pnpm/global/5/node_modules/carrick", "pnpm add -g carrick@latest"],
    ["npm", "/usr/local/lib/node_modules/carrick", "npm install -g carrick@latest"],
    ["npm", "/Users/dev/.nvm/versions/node/v24.3.0/lib/node_modules/carrick", "npm install -g carrick@latest"],
  ];
  for (const [kind, directory, command] of cases) {
    const shape = installShape(directory.split("/").join(path.sep), {});
    assert.equal(shape.kind, kind, directory);
    assert.equal(shape.command, command, directory);
    assert.equal(shape.manifest, undefined, directory);
  }
});

test("a project dependency is named with its manifest and its package manager", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-shape-"));
  try {
    const installed = path.join(root, "node_modules", "carrick");
    fs.mkdirSync(installed, { recursive: true });
    fs.writeFileSync(
      path.join(root, "package.json"),
      JSON.stringify({ name: "api", devDependencies: { carrick: "0.3.68" } }),
    );

    let shape = installShape(installed, {});
    assert.equal(shape.kind, "project");
    assert.equal(shape.command, "npm install -D carrick@latest");
    assert.equal(shape.manifest, path.join(root, "package.json"));

    fs.writeFileSync(path.join(root, "pnpm-lock.yaml"), "");
    assert.equal(installShape(installed, {}).command, "pnpm add -D carrick@latest");

    // A pnpm store path still resolves to the repository that encloses it.
    const nested = path.join(root, "node_modules", ".pnpm", "carrick@0.3.68", "node_modules", "carrick");
    fs.mkdirSync(nested, { recursive: true });
    assert.equal(installShape(nested, {}).manifest, path.join(root, "package.json"));

    // A repository that does not depend on carrick is not the reason this copy
    // is here — that is a global install sitting inside somebody's home.
    fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ name: "api" }));
    assert.equal(installShape(installed, {}).kind, "npm");
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("the notice carries both versions and a runnable command, or is not printed", () => {
  const root = "/usr/local/lib/node_modules/carrick".split("/").join(path.sep);
  assert.equal(updateNotice(null, "0.3.73", { env: {}, root }), null, "no current version");
  assert.equal(updateNotice("0.3.68", null, { env: {}, root }), null, "nothing known about the registry");
  assert.equal(updateNotice("0.3.73", "0.3.73", { env: {}, root }), null, "already current");
  assert.equal(updateNotice("0.3.74", "0.3.73", { env: {}, root }), null, "ahead of the registry");

  const line = updateNotice("0.3.68", "0.3.73", { env: {}, root });
  assert.ok(line);
  assert.match(line, /carrick 0\.3\.68 is installed/);
  assert.match(line, /0\.3\.73 is published/);
  assert.match(line, /npm install -g carrick@latest/);
  // No off switch in the line itself: the session-start hook puts this on
  // stdout, straight into an agent's context, and the failure it exists for is
  // an agent that does not upgrade.
  assert.doesNotMatch(line, /CARRICK_NO_UPDATE_CHECK/);
});

test("on GitHub Actions the notice is an annotation that says the run continues", () => {
  const line = updateNotice("0.3.68", "0.3.73", { env: { GITHUB_ACTIONS: "true" }, root: "/anywhere" });
  assert.ok(line);
  assert.ok(line.startsWith("::warning::"), line);
  assert.match(line, /This run continues on 0\.3\.68/);
  // CI is never told to install anything: the workflow decides what runs.
  assert.doesNotMatch(line, /npm install/);
  // A workflow that calls the CLI directly wrote a version string of its own.
  assert.match(line, /carrick@latest/);
});

test("a run inside the Carrick action is told about the ref, not about a version string", () => {
  const line = updateNotice("0.3.68", "0.3.73", {
    env: { GITHUB_ACTIONS: "true", GITHUB_ACTION_REPOSITORY: "carrick-tools/carrick" },
    root: "/anywhere",
  });
  assert.ok(line);
  assert.match(line, /pins the Carrick action to a ref that does not move/);
  assert.match(line, /carrick-tools\/carrick@v1/);
  // The user's workflow contains no `carrick@<version>` line to change.
  assert.doesNotMatch(line, /Run `carrick@latest`/);
});

test("fetchLatest answers null for every unhappy registry", async () => {
  assert.equal(await fetchLatest(stubFetch({ ok: true, json: async () => ({ latest: "0.3.73" }) }).impl), "0.3.73");
  assert.equal(await fetchLatest(stubFetch({ ok: false }).impl), null, "a 500");
  assert.equal(
    await fetchLatest(stubFetch({ ok: true, json: async () => ({ latest: 73 }) }).impl),
    null,
    "latest of the wrong type",
  );
  assert.equal(
    await fetchLatest(stubFetch({ ok: true, json: async () => ({ latest: "next" }) }).impl),
    null,
    "a tag that is not a version",
  );
  assert.equal(
    await fetchLatest((() => Promise.reject(new Error("ENOTFOUND"))) as unknown as typeof fetch),
    null,
    "no network",
  );
  assert.equal(
    await fetchLatest(
      stubFetch({
        ok: true,
        json: async () => {
          throw new Error("not json");
        },
      }).impl,
    ),
    null,
    "a body that is not JSON",
  );
});

test("the background check is started at most once a TTL, and never in CI", () => {
  const box = sandbox();
  const started: string[][] = [];
  const options: Array<Record<string, unknown>> = [];
  const spawnImpl = ((command: string, args: string[], opts: Record<string, unknown>) => {
    started.push([command, ...args]);
    options.push(opts);
    return { unref: () => {} };
  }) as unknown as typeof import("node:child_process").spawn;
  try {
    assert.equal(scheduleUpdateCheck({ ...box.env, CI: "true" }, spawnImpl), false, "CI");
    assert.equal(
      scheduleUpdateCheck({ ...box.env, CARRICK_NO_UPDATE_CHECK: "1" }, spawnImpl),
      false,
      "suppressed",
    );
    // `assert.equal` on the length, not deepEqual on the array: deepEqual
    // against a literal narrows `started` to never[] for the rest of the test.
    assert.equal(started.length, 0);

    assert.equal(scheduleUpdateCheck(box.env, spawnImpl), true, "first run, nothing cached");
    assert.equal(started.length, 1);
    assert.ok(started[0]![1]!.endsWith("update-check.js"), started[0]![1]);
    // A detached child on Windows gets its own console window without this,
    // and win32-x64 is a platform this package publishes.
    assert.equal(options[0]!["windowsHide"], true);
    assert.equal(options[0]!["detached"], true);
    assert.equal(options[0]!["stdio"], "ignore");

    // The parent stamped the cache before spawning, so the next invocation in
    // the same window does not fork a second child even though the child has
    // not answered yet.
    assert.equal(scheduleUpdateCheck(box.env, spawnImpl), false, "inside the TTL");
    assert.equal(started.length, 1);

    // A previously known version survives the stamp: a failed check must not
    // make the machine forget it is behind.
    writeUpdateState({ checked_at: "2000-01-01T00:00:00Z", latest: "0.9.9" }, box.env);
    assert.equal(scheduleUpdateCheck(box.env, spawnImpl), true, "stale again");
    assert.equal(readUpdateState(box.env)?.latest, "0.9.9");
  } finally {
    box.dispose();
  }
});

test("a config directory nobody can write forks nothing", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-update-"));
  const blocked = path.join(root, "blocker");
  fs.writeFileSync(blocked, "not a directory");
  let started = 0;
  const spawnImpl = (() => {
    started += 1;
    return { unref: () => {} };
  }) as unknown as typeof import("node:child_process").spawn;
  try {
    assert.equal(scheduleUpdateCheck({ XDG_CONFIG_HOME: blocked }, spawnImpl), false);
    assert.equal(started, 0);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("CI pays for a synchronous check in front of a scan and nothing else", async () => {
  const fetchOk = stubFetch({ ok: true, json: async () => ({ latest: "99.0.0" }) });
  const env = { CI: "true", GITHUB_ACTIONS: "true" };

  assert.equal(await ciUpdateNotice(["status"], env, fetchOk.impl), null);
  assert.equal(await ciUpdateNotice(["--version"], env, fetchOk.impl), null);
  assert.equal(await ciUpdateNotice([], env, fetchOk.impl), null);
  assert.equal(fetchOk.calls, 0, "no scan, no fetch");

  // A laptop never takes this path, however the arguments read.
  assert.equal(await ciUpdateNotice(["index"], {}, fetchOk.impl), null);
  assert.equal(fetchOk.calls, 0);

  const scan = await ciUpdateNotice(["index"], env, fetchOk.impl);
  assert.ok(scan?.startsWith("::warning::"), String(scan));
  // A bare path is how the scanner is asked to scan a directory.
  assert.ok((await ciUpdateNotice(["./packages/api"], env, fetchOk.impl))?.startsWith("::warning::"));
  assert.equal(await ciUpdateNotice(["index"], { ...env, CARRICK_NO_UPDATE_CHECK: "1" }, fetchOk.impl), null);
});

test("which invocations count as a scan", () => {
  assert.equal(isScanInvocation(["index"]), true);
  assert.equal(isScanInvocation(["."]), true);
  assert.equal(isScanInvocation(["packages/api"]), true);
  assert.equal(isScanInvocation([]), false);
  assert.equal(isScanInvocation(["--help"]), false);
  // Every name either half of the CLI answers, including the binary's own
  // LOCAL_COMMANDS (src/local_mode/cli.rs).
  for (const command of [
    "derive",
    "refresh",
    "status",
    "check",
    "touch",
    "login",
    "logout",
    "lsp",
    "hook",
    "init",
    "remove",
    "doctor",
    "templates",
  ]) {
    assert.equal(isScanInvocation([command]), false, command);
  }
});

test("this package can read its own version", () => {
  assert.match(String(currentVersion()), /^\d+\.\d+\.\d+/);
  assert.equal(currentVersion(path.join(os.tmpdir(), "carrick-nothing-here")), null);
});

test("doctor's version check: a finding when behind, a note when the registry is silent", async () => {
  const box = sandbox();
  const now = Date.parse("2026-09-16T12:00:00Z");
  try {
    assert.deepEqual(await checkVersion({ ...box.env, CARRICK_NO_UPDATE_CHECK: "1" }, fetch, now), []);

    const behind = await checkVersion(box.env, stubFetch({ ok: true, json: async () => ({ latest: "99.0.0" }) }).impl, now);
    assert.equal(behind.length, 1);
    assert.equal(behind[0]!.level, "warn", behind[0]!.text);
    assert.match(behind[0]!.text, /99\.0\.0 is published/);
    assert.match(behind[0]!.text, /carrick@latest/);
    // The live answer is cached, so the shim's next notice does not wait for
    // its own check.
    assert.equal(readUpdateState(box.env)?.latest, "99.0.0");

  } finally {
    box.dispose();
  }

  const fresh = sandbox();
  try {
    const current = await checkVersion(fresh.env, stubFetch({ ok: true, json: async () => ({ latest: "0.0.1" }) }).impl, now);
    assert.equal(current.length, 1);
    assert.equal(current[0]!.level, "done", current[0]!.text);
  } finally {
    fresh.dispose();
  }

  const silent = sandbox();
  try {
    const lines = await checkVersion(silent.env, stubFetch({ ok: false }).impl, now);
    assert.equal(lines.length, 1);
    assert.equal(lines[0]!.level, "say", lines[0]!.text);
    assert.match(lines[0]!.text, /did not answer/);
  } finally {
    silent.dispose();
  }
});
