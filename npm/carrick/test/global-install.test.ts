// Leaving no older `carrick` on PATH behind an npx run (carrick#1372).
//
// The claims under test: an older global is upgraded without being asked
// about, through the manager that owns it and with no flag of ours overriding
// that machine's configuration; the result is verified by resolving `carrick`
// again; a machine with no global is never installed onto without consent; and
// none of it can fail the command somebody typed.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
  failureReason,
  findCarrick,
  firstGlobalCommand,
  globalCommand,
  installedVersion,
  offerGlobalInstall,
  syncGlobalInstall,
  syncsOnThisRun,
  type GlobalCarrick,
  type GlobalMachine,
} from "../src/global-install.ts";
import type { InstallShape } from "../src/update.ts";

const packageDir = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const posix = { skip: process.platform === "win32" ? "these fixtures need a POSIX shebang" : false };

function temporary(prefix: string): string {
  return fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
}

/**
 * A machine whose `carrick` is at `versions[n]` on the nth lookup, recording
 * what was installed.
 *
 * One entry per lookup because the verification is a second lookup: the pair
 * `["0.3.81", "0.3.84"]` is an install that took, and `["0.3.81", "0.3.81"]` is
 * one that reported success and changed nothing. A missing entry is a machine
 * with no `carrick` on PATH at that moment.
 */
function machineWith(options: {
  versions: Array<string | null | undefined>;
  kind?: InstallShape["kind"];
  manifest?: string;
  ok?: boolean;
  reason?: string;
  binary?: string;
}): { machine: GlobalMachine; installs: string[][] } {
  const state = { finds: 0 };
  const installs: string[][] = [];
  const machine: GlobalMachine = {
    find: () => {
      const version = options.versions[state.finds];
      state.finds += 1;
      if (version === undefined) return null;
      return {
        binary: options.binary ?? "/usr/local/bin/carrick",
        real: "/usr/local/lib/node_modules/carrick/bin/carrick.mjs",
        version,
      };
    },
    shape: () => ({
      kind: options.kind ?? "npm",
      command: "npm install -g carrick@latest",
      ...(options.manifest ? { manifest: options.manifest } : {}),
    }),
    install: (argv) => {
      installs.push(argv);
      return options.ok === false
        ? { ok: false, reason: options.reason ?? "it refused" }
        : { ok: true, reason: null };
    },
  };
  return { machine, installs };
}

test("an older global is upgraded to the running version, and the run says so", () => {
  const said: string[] = [];
  const { machine, installs } = machineWith({ versions: ["0.3.81", "0.3.84"] });
  const outcome = syncGlobalInstall({ running: "0.3.84", machine, say: (line) => said.push(line) });
  assert.deepEqual(outcome, { kind: "upgraded", from: "0.3.81", to: "0.3.84" });
  assert.deepEqual(installs, [["npm", "install", "-g", "carrick@0.3.84"]]);
  assert.equal(said.length, 2, said.join(" | "));
  assert.match(said[0]!, /Upgrading the global carrick 0\.3\.81 -> 0\.3\.84/);
  assert.match(said[1]!, /now 0\.3\.84/);
});

test("nothing of ours overrides the machine's own package-manager configuration", () => {
  // A user-level release-age policy, a private registry or a prefix is that
  // machine's decision. The proof that we do not reach past it is that the
  // command carries nothing but the manager, the global flag and the version.
  for (const kind of ["npm", "pnpm", "bun", "volta"] as const) {
    const argv = globalCommand(kind, "0.3.84")!;
    assert.equal(argv[argv.length - 1], "carrick@0.3.84");
    for (const flag of ["--registry", "--prefix", "--before", "--min-release-age", "--userconfig", "--force"]) {
      assert.equal(argv.includes(flag), false, `${kind} passes ${flag}`);
    }
  }
  assert.deepEqual(globalCommand("pnpm", "1.2.3"), ["pnpm", "add", "-g", "carrick@1.2.3"]);
  assert.deepEqual(globalCommand("bun", "1.2.3"), ["bun", "add", "-g", "carrick@1.2.3"]);
  assert.deepEqual(globalCommand("volta", "1.2.3"), ["volta", "install", "carrick@1.2.3"]);
  // Neither of these is a global install this run can replace.
  assert.equal(globalCommand("npx", "1.2.3"), null);
  assert.equal(globalCommand("project", "1.2.3"), null);
  // And a machine with no global at all is offered npm, whatever ran this.
  assert.deepEqual(firstGlobalCommand("npx", "1.2.3"), ["npm", "install", "-g", "carrick@1.2.3"]);
  assert.deepEqual(firstGlobalCommand("bun", "1.2.3"), ["bun", "add", "-g", "carrick@1.2.3"]);
});

test("a refused install prints the reason and one command, and does not retry", () => {
  const said: string[] = [];
  const { machine, installs } = machineWith({
    versions: ["0.3.81"],
    ok: false,
    reason: "npm error notarget No matching version found for carrick@0.3.84 with a date before …",
  });
  const outcome = syncGlobalInstall({ running: "0.3.84", machine, say: (line) => said.push(line) });
  assert.equal(outcome.kind, "refused");
  assert.equal(installs.length, 1, "a refusal is not retried with more force");
  assert.match(said[1]!, /still 0\.3\.81: npm error notarget/);
  assert.match(said[1]!, /npm install -g carrick@0\.3\.84/);
});

test("an install that reported success but left an older carrick first on PATH is named", () => {
  const said: string[] = [];
  // The second `find` is the verification: a second install earlier on PATH,
  // or a version manager's shim, answers first and was never touched.
  const { machine } = machineWith({ versions: ["0.3.81", "0.3.81"], binary: "/opt/shim/carrick" });
  const outcome = syncGlobalInstall({ running: "0.3.84", machine, say: (line) => said.push(line) });
  assert.equal(outcome.kind, "stale");
  assert.match(said[1]!, /still runs 0\.3\.81 from \/opt\/shim\/carrick/);
  assert.match(said[1]!, /npm install -g carrick@0\.3\.84/);
});

test("an install that put nothing on PATH says which directory is missing from it", () => {
  const said: string[] = [];
  const { machine } = machineWith({ versions: ["0.3.81"] });
  const outcome = syncGlobalInstall({ running: "0.3.84", machine, say: (line) => said.push(line) });
  assert.equal(outcome.kind, "stale");
  assert.match(said[1]!, /nothing answers to `carrick` on PATH/);
});

test("a current, newer or unreadable global is left alone and says nothing", () => {
  for (const version of ["0.3.84", "0.4.0", null]) {
    const said: string[] = [];
    const { machine, installs } = machineWith({ versions: [version] });
    const outcome = syncGlobalInstall({ running: "0.3.84", machine, say: (line) => said.push(line) });
    assert.notEqual(outcome.kind, "upgraded");
    assert.deepEqual(installs, [], `${version} was installed over`);
    assert.deepEqual(said, [], `${version} printed ${said.join(" | ")}`);
  }
});

test("a global that is a project dependency is reported, never installed over", () => {
  const said: string[] = [];
  const { machine, installs } = machineWith({
    versions: ["0.3.81"],
    kind: "project",
    manifest: "/repo/package.json",
  });
  const outcome = syncGlobalInstall({ running: "0.3.84", machine, say: (line) => said.push(line) });
  assert.equal(outcome.kind, "stale");
  assert.deepEqual(installs, []);
  assert.match(said[0]!, /pinned in \/repo\/package\.json/);
});

test("nothing on PATH is not a reason to install anything", () => {
  const said: string[] = [];
  const { machine, installs } = machineWith({ versions: [] });
  const outcome = syncGlobalInstall({ running: "0.3.84", machine, say: (line) => said.push(line) });
  assert.deepEqual(outcome, { kind: "skipped", why: "no carrick on PATH" });
  assert.deepEqual(installs, []);
  assert.deepEqual(said, []);
});

test("the sync runs on a command started through npx, and on nothing else", () => {
  const npx = path.join(os.homedir(), ".npm", "_npx", "abc123", "node_modules", "carrick");
  const global = path.join("/usr", "local", "lib", "node_modules", "carrick");
  const env = { PATH: "/usr/bin" };
  for (const command of ["index", "status", "refresh", "resume", "init", "doctor"]) {
    assert.equal(syncsOnThisRun(command, env, npx), true, `${command} through npx`);
  }
  for (const command of ["hook", "lsp", "remove", "--version", "-V", "--help", "-h", undefined]) {
    assert.equal(syncsOnThisRun(command, env, npx), false, `${command} through npx`);
  }
  assert.equal(syncsOnThisRun("index", env, global), false, "a global install is what it would upgrade");
  assert.equal(syncsOnThisRun("index", { ...env, CI: "true" }, npx), false, "CI");
  assert.equal(syncsOnThisRun("index", { ...env, GITHUB_ACTIONS: "true" }, npx), false, "Actions");
  assert.equal(syncsOnThisRun("index", { ...env, CARRICK_NO_UPDATE_CHECK: "1" }, npx), false, "suppressed");
});

test("`which` cannot answer this: the npx cache's own shim is not a global", () => {
  const home = temporary("carrick-path-");
  const cache = path.join(home, ".npm", "_npx", "abc123", "node_modules", ".bin");
  const real = path.join(home, "global", "bin");
  fs.mkdirSync(cache, { recursive: true });
  fs.mkdirSync(real, { recursive: true });
  fs.writeFileSync(path.join(cache, "carrick"), "#!/bin/sh\n");
  fs.writeFileSync(path.join(real, "carrick"), "#!/bin/sh\n");
  try {
    // npm puts the exec tree's `.bin` first, which is exactly the entry that
    // must be skipped: it stops existing when the npx run ends.
    const found = findCarrick({
      env: { PATH: [cache, real].join(path.delimiter) },
      ours: path.join(home, ".npm", "_npx", "abc123", "node_modules", "carrick"),
      platform: "linux",
    });
    assert.equal(found?.binary, path.join(real, "carrick"));
    // And a PATH holding only the npx entry has no global on it at all.
    assert.equal(findCarrick({ env: { PATH: cache }, platform: "linux" }), null);
    // As does the copy running this test, wherever it is installed.
    assert.equal(findCarrick({ env: { PATH: real }, ours: home, platform: "linux" }), null);
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test("a global's version is read off its own manifest before anything is spawned", () => {
  const home = temporary("carrick-version-");
  const root = path.join(home, "lib", "node_modules", "carrick");
  fs.mkdirSync(path.join(root, "bin"), { recursive: true });
  fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ name: "carrick", version: "0.3.81" }));
  const found: GlobalCarrick = {
    binary: path.join(home, "bin", "carrick"),
    real: path.join(root, "bin", "carrick.mjs"),
    version: null,
  };
  try {
    assert.equal(
      installedVersion(found, {
        run: () => {
          throw new Error("the manifest answered; nothing should be spawned");
        },
      }),
      "0.3.81",
    );
    // A manifest of somebody else's package is not an answer.
    fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ name: "other", version: "9.9.9" }));
    assert.equal(installedVersion(found, { run: () => "0.3.81" }), "0.3.81");
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test("a failed install is quoted by its last real line, not by npm's warnings", () => {
  assert.equal(
    failureReason({
      status: 1,
      stdout: "",
      stderr: "npm warn cli npm v12 does not support this node\nnpm error code ETARGET\nnpm error notarget no version\n",
    }),
    "npm error notarget no version",
  );
  assert.equal(failureReason({ status: 127, stdout: "", stderr: "" }), "it exited 127");
  assert.equal(
    failureReason({ status: null, stdout: null, stderr: null, error: new Error("spawn npm ENOENT") }),
    "spawn npm ENOENT",
  );
});

test("with no global, --yes prints the command and installs nothing", async () => {
  const said: string[] = [];
  const { machine, installs } = machineWith({ versions: [] });
  const outcome = await offerGlobalInstall({
    running: "0.3.84",
    assumeYes: true,
    install: false,
    confirm: async () => {
      throw new Error("--yes is an answer about the workspace, not about this machine");
    },
    say: (line) => said.push(line),
    machine,
  });
  assert.equal(outcome, "declined");
  assert.deepEqual(installs, []);
  assert.match(said[0]!, /not on PATH/);
  assert.match(said[0]!, /npm install -g carrick@0\.3\.84/);
});

test("with no global, --install-global installs without asking", async () => {
  const said: string[] = [];
  const { machine, installs } = machineWith({ versions: [undefined, "0.3.84"] });
  const outcome = await offerGlobalInstall({
    running: "0.3.84",
    assumeYes: true,
    install: true,
    confirm: async () => false,
    say: (line) => said.push(line),
    machine,
  });
  assert.equal(outcome, "installed");
  assert.deepEqual(installs, [["npm", "install", "-g", "carrick@0.3.84"]]);
});

test("with no global and a terminal, the answer to the question decides", async () => {
  for (const answer of [true, false]) {
    const said: string[] = [];
    const { machine, installs } = machineWith({
      versions: [undefined, "0.3.84"],
    });
    const outcome = await offerGlobalInstall({
      running: "0.3.84",
      assumeYes: false,
      install: false,
      confirm: async (question) => {
        assert.match(question, /npm install -g carrick@0\.3\.84/);
        return answer;
      },
      say: (line) => said.push(line),
      machine,
    });
    assert.equal(outcome, answer ? "installed" : "declined");
    assert.equal(installs.length, answer ? 1 : 0);
  }
});

test("a machine that already has a global is not offered one", async () => {
  const { machine, installs } = machineWith({ versions: ["0.3.84"] });
  const outcome = await offerGlobalInstall({
    running: "0.3.84",
    assumeYes: false,
    install: true,
    confirm: async () => true,
    say: () => {},
    machine,
  });
  assert.equal(outcome, "present");
  assert.deepEqual(installs, []);
});

/**
 * The shim, run the way npx runs it, against a fake global and a fake npm.
 *
 * Everything above this is the decision; this is the wiring. It proves the
 * three things a unit test cannot: that the shim calls the sync at all, that
 * the npx shape is detected from the path the package sits at, and that the
 * verification resolves `carrick` a second time on a real PATH.
 */
function npxTree(carrickPrints: string, npmScript: string): {
  entry: string;
  env: NodeJS.ProcessEnv;
  home: string;
  log: string;
  running: string;
  cleanup: () => void;
} {
  const home = temporary("carrick-npx-");
  const tree = path.join(home, ".npm", "_npx", "abc123", "node_modules", "carrick");
  fs.mkdirSync(tree, { recursive: true });
  for (const entry of ["bin", "dist", "templates", "package.json"]) {
    fs.cpSync(path.join(packageDir, entry), path.join(tree, entry), { recursive: true });
  }
  fs.symlinkSync(path.join(packageDir, "node_modules"), path.join(tree, "node_modules"));
  const running = JSON.parse(fs.readFileSync(path.join(tree, "package.json"), "utf8")).version;

  const bin = path.join(home, "bin");
  fs.mkdirSync(bin, { recursive: true });
  const log = path.join(home, "npm-argv.log");
  fs.writeFileSync(path.join(bin, "carrick"), carrickPrints, { mode: 0o755 });
  fs.writeFileSync(path.join(bin, "npm"), npmScript, { mode: 0o755 });

  // The published-version check is a different question and is answered here
  // before the run asks it: a cache stamped now, naming this version as the
  // published one, leaves nothing stale to refresh. Without it this one test
  // would be the one that dials the registry and forks a child to do it.
  const config = path.join(home, "config", "carrick");
  fs.mkdirSync(config, { recursive: true });
  fs.writeFileSync(
    path.join(config, "update-check.json"),
    JSON.stringify({ checked_at: new Date().toISOString(), latest: running }),
  );

  const env: NodeJS.ProcessEnv = {
    ...process.env,
    PATH: [bin, "/usr/bin", "/bin"].join(path.delimiter),
    XDG_CONFIG_HOME: path.join(home, "config"),
    CARRICK_LOG_QUIET: "1",
    CARRICK_VERSION_FILE: path.join(bin, "carrick"),
    CARRICK_ARGV_LOG: log,
  };
  // The suite turns the version check off for every other test; this one is
  // about what happens when it is on, and CI is not a machine with a global.
  delete env["CARRICK_NO_UPDATE_CHECK"];
  delete env["CI"];
  delete env["GITHUB_ACTIONS"];
  return {
    entry: path.join(tree, "bin", "carrick.mjs"),
    env,
    home,
    log,
    running,
    cleanup: () => fs.rmSync(home, { recursive: true, force: true }),
  };
}

test("an npx run upgrades the older global it finds on PATH", posix, () => {
  const fixture = npxTree(
    '#!/bin/sh\ncat "$CARRICK_VERSION_FILE.version" 2>/dev/null || echo 0.0.1\n',
    // The install a real npm would do: the binary now answers with the version
    // it was asked for.
    '#!/bin/sh\necho "$@" >> "$CARRICK_ARGV_LOG"\nfor word in "$@"; do last=$word; done\necho "${last#carrick@}" > "$CARRICK_VERSION_FILE.version"\n',
  );
  try {
    // `templates` because it is a command that does the sync and then answers
    // without the scanner binary, an index or a network: what is under test is
    // the two lines on stderr in front of whatever the command does.
    const answer = spawnSync(process.execPath, [fixture.entry, "templates", "workflow"], {
      encoding: "utf8",
      env: fixture.env,
      cwd: fixture.home,
      timeout: 60_000,
    });
    assert.equal(
      fs.readFileSync(fixture.log, "utf8").trim(),
      `install -g carrick@${fixture.running}`,
      answer.stderr,
    );
    assert.match(answer.stderr, /Upgrading the global carrick 0\.0\.1 -> /);
    assert.match(answer.stderr, new RegExp(`now ${fixture.running.replace(/\./g, "\\.")}`));
  } finally {
    fixture.cleanup();
  }
});

test("an npx run that could not replace the older global still names it", posix, () => {
  const fixture = npxTree(
    "#!/bin/sh\necho 0.0.1\n",
    // An install that says it worked and changes nothing: the shape of a second
    // install earlier on PATH, or of a version manager's shim.
    '#!/bin/sh\necho "$@" >> "$CARRICK_ARGV_LOG"\n',
  );
  try {
    const answer = spawnSync(process.execPath, [fixture.entry, "templates", "workflow"], {
      encoding: "utf8",
      env: fixture.env,
      cwd: fixture.home,
      timeout: 60_000,
    });
    assert.match(answer.stderr, /still runs 0\.0\.1 from /, answer.stderr);
  } finally {
    fixture.cleanup();
  }
});
