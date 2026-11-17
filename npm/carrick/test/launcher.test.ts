// The hook launcher (carrick#4759): the real sh script, run against fake home
// directories laid out the way each Node version manager lays out a global
// install. The lookup lives in `templates/carrick-hook.sh` only, so this is
// what proves it; there is no TypeScript copy of it to test instead.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { launcherPath, installNode, LAUNCHER_FILE } from "../src/init/launcher.ts";
import { carrickHooks, isOurs, mergeCarrickHooks, removeCarrickHooks } from "../src/init/settings.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..", "..", "..");

/** A home directory, and a PATH that holds no carrick. */
function home(t: test.TestContext): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "launcher-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return fs.realpathSync(dir);
}

/** A carrick stand-in that says which one it is and which bin came first on PATH. */
function stub(file: string, name: string, exitCode = 0): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `#!/bin/sh\necho "${name} $* first=\${PATH%%:*}"\nexit ${exitCode}\n`, { mode: 0o755 });
}

function run(
  homeDir: string,
  args: string[],
  extra: NodeJS.ProcessEnv = {},
): { status: number | null; stdout: string; stderr: string } {
  const env: NodeJS.ProcessEnv = { HOME: homeDir, PATH: "/usr/bin:/bin", ...extra };
  const result = spawnSync("sh", [launcherPath(), ...args], { encoding: "utf8", env, cwd: homeDir });
  return { status: result.status, stdout: result.stdout, stderr: result.stderr };
}

test("nvm: a global install under a Node that is not this shell's still runs", (t) => {
  const h = home(t);
  const bin = path.join(h, ".nvm", "versions", "node", "v24.9.0", "bin");
  stub(path.join(bin, "carrick"), "nvm");
  const out = run(h, ["hook", "post-edit"]);
  assert.equal(out.status, 0);
  // Its own bin directory first, so `#!/usr/bin/env node` finds the Node it was installed under.
  assert.equal(out.stdout.trim(), `nvm hook post-edit first=${bin}`);
});

test("fnm, in both of its directory layouts", (t) => {
  for (const relative of [
    [".local", "share", "fnm", "node-versions", "v22.1.0", "installation", "bin"],
    ["Library", "Application Support", "fnm", "node-versions", "v22.1.0", "installation", "bin"],
  ]) {
    const h = home(t);
    stub(path.join(h, ...relative, "carrick"), "fnm");
    assert.match(run(h, ["hook", "stop"]).stdout, /^fnm hook stop /);
  }
  const h = home(t);
  stub(path.join(h, "fnm-here", "node-versions", "v22.1.0", "installation", "bin", "carrick"), "fnm-dir");
  assert.match(run(h, ["hook", "stop"], { FNM_DIR: path.join(h, "fnm-here") }).stdout, /^fnm-dir /);
});

test("volta, asdf, mise and a project-local install", (t) => {
  const cases: Array<[string[], string, NodeJS.ProcessEnv?]> = [
    [[".volta", "bin", "carrick"], "volta"],
    [[".asdf", "installs", "nodejs", "24.9.0", "bin", "carrick"], "asdf"],
    [[".local", "share", "mise", "installs", "node", "24.9.0", "bin", "carrick"], "mise"],
  ];
  for (const [relative, name] of cases) {
    const h = home(t);
    stub(path.join(h, ...relative), name);
    assert.match(run(h, ["hook", "post-read"]).stdout, new RegExp(`^${name} `));
  }
  const h = home(t);
  stub(path.join(h, "project", "node_modules", ".bin", "carrick"), "local");
  assert.match(run(h, ["hook", "post-read"], { CLAUDE_PROJECT_DIR: path.join(h, "project") }).stdout, /^local /);
});

test("the npm global prefix: by environment, and by ~/.npmrc", (t) => {
  const h = home(t);
  stub(path.join(h, "globals", "bin", "carrick"), "env-prefix");
  assert.match(run(h, ["hook", "stop"], { NPM_CONFIG_PREFIX: path.join(h, "globals") }).stdout, /^env-prefix /);

  const g = home(t);
  stub(path.join(g, "mine", "bin", "carrick"), "npmrc-prefix");
  fs.writeFileSync(path.join(g, ".npmrc"), "registry=https://example.invalid\nprefix=~/mine\n");
  assert.match(run(g, ["hook", "stop"]).stdout, /^npmrc-prefix /);
});

test("the newest Node wins, compared as numbers", (t) => {
  const h = home(t);
  for (const version of ["v9.11.2", "v24.10.0", "v24.9.0", "v100.0.0-rc.1"]) {
    stub(path.join(h, ".nvm", "versions", "node", version, "bin", "carrick"), version);
  }
  assert.match(run(h, ["hook", "stop"]).stdout, /^v100\.0\.0-rc\.1 /);
  fs.rmSync(path.join(h, ".nvm", "versions", "node", "v100.0.0-rc.1"), { recursive: true });
  assert.match(run(h, ["hook", "stop"]).stdout, /^v24\.10\.0 /);
});

test("a carrick on PATH is used first, with nothing looked up", (t) => {
  const h = home(t);
  stub(path.join(h, ".nvm", "versions", "node", "v24.9.0", "bin", "carrick"), "nvm");
  stub(path.join(h, "onpath", "carrick"), "path");
  assert.match(run(h, ["hook", "stop"], { PATH: `${path.join(h, "onpath")}:/usr/bin:/bin` }).stdout, /^path hook stop /);
});

test("what the found carrick exits with is what the hook exits with", (t) => {
  const h = home(t);
  stub(path.join(h, ".volta", "bin", "carrick"), "volta", 2);
  assert.equal(run(h, ["hook", "post-edit"]).status, 2);
});

test("nothing found: one line at session start, nothing for any other hook, exit 0", (t) => {
  const h = home(t);
  const start = run(h, ["hook", "session-start"], { CLAUDE_PROJECT_DIR: h });
  assert.equal(start.status, 0);
  assert.equal(start.stdout.split("\n").filter((line) => line !== "").length, 1);
  assert.match(start.stdout, /Carrick is not installed in this shell; agent hooks are off/);
  assert.match(start.stdout, /npm install -g carrick/);
  assert.equal(start.stderr, "");

  for (const event of ["post-edit", "post-read", "post-bash", "post-search", "stop", "subagent-stop", "user-prompt"]) {
    const other = run(h, ["hook", event], { CLAUDE_PROJECT_DIR: h });
    assert.equal(other.status, 0, event);
    assert.equal(other.stdout, "", event);
    assert.equal(other.stderr, "", event);
  }
});

test("nothing found under Codex: the session-start line is the JSON Codex reads", (t) => {
  const h = home(t);
  const out = run(h, ["hook", "session-start"]);
  assert.equal(out.status, 0);
  const parsed = JSON.parse(out.stdout) as { hookSpecificOutput: { hookEventName: string; additionalContext: string } };
  assert.equal(parsed.hookSpecificOutput.hookEventName, "SessionStart");
  assert.match(parsed.hookSpecificOutput.additionalContext, /npm install -g carrick/);
});

test("--where says which carrick a hook would run, and nothing when there is none", (t) => {
  const h = home(t);
  assert.equal(run(h, ["--where"]).stdout, "");
  const binary = path.join(h, ".nvm", "versions", "node", "v24.9.0", "bin", "carrick");
  stub(binary, "nvm");
  assert.equal(run(h, ["--where"]).stdout.trim(), binary);
  assert.equal(installNode(binary), "24.9.0");
  assert.equal(installNode("/usr/local/bin/carrick"), null);
});

test("the plugin's copy of the launcher is the package's, byte for byte", () => {
  assert.equal(
    fs.readFileSync(path.join(repoRoot, "plugin", "hooks", LAUNCHER_FILE), "utf8"),
    fs.readFileSync(launcherPath(), "utf8"),
  );
});

test("the plugin's hook entries all call the launcher and keep their events", () => {
  const hooks = JSON.parse(fs.readFileSync(path.join(repoRoot, "plugin", "hooks", "hooks.json"), "utf8")) as {
    hooks: Record<string, Array<{ hooks: Array<{ command: string }> }>>;
  };
  const commands = Object.values(hooks.hooks).flatMap((groups) => groups.flatMap((group) => group.hooks.map((h) => h.command)));
  assert.equal(commands.length, 8);
  for (const command of commands) {
    assert.match(command, /^sh "\$\{CLAUDE_PLUGIN_ROOT\}\/hooks\/carrick-hook\.sh" hook [a-z-]+$/);
    assert.equal(isOurs({ type: "command", command }), true);
  }
});

test("init's entries call the launcher, replace the old bare ones, and are still removable", () => {
  for (const groups of Object.values(carrickHooks())) {
    for (const group of groups) {
      for (const entry of group.hooks) {
        assert.match(entry.command, /^sh "\$CLAUDE_PROJECT_DIR\/\.claude\/carrick-hook\.sh" hook [a-z-]+$/);
        assert.equal(isOurs(entry), true);
      }
    }
  }
  // A file an older version wrote: bare, and absolute-path entries.
  for (const old of ["carrick", "/opt/my tools/carrick/bin/carrick.mjs"]) {
    const before = mergeCarrickHooks(null, /\s/.test(old) ? `"${old}"` : old).body;
    const after = JSON.parse(mergeCarrickHooks(before).body) as { hooks: Record<string, unknown[]> };
    assert.equal(after.hooks["PostToolUse"]!.length, 4, "one entry per matcher, not two");
    assert.doesNotMatch(JSON.stringify(after), /"command": "carrick hook/);
    // Both forms come out again.
    assert.equal(removeCarrickHooks(before).changed, true);
    assert.equal(removeCarrickHooks(mergeCarrickHooks(before).body).changed, true);
  }
});
