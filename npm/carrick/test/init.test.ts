// What `carrick init` decides before it writes anything, and what it writes.
//
// The two that matter most are the two that can destroy something: the repo
// list a user edited by hand, and a settings file that already holds someone
// else's hooks. Both are merges, and both are tested for what they leave alone.
// There is one run (carrick#3361): init reads which repos the workspace holds,
// asks nothing after sign-in, and writes no config and no proposal.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  deriveWorkspace,
  ensureIndexDir,
  repoIdentity,
  siblingRepos,
  type WorkspaceProposal,
} from "../src/init/repos.ts";
import {
  carrickHooks,
  hookCommand,
  mergeCarrickHooks,
  ownEntryPoint,
  removeCarrickHooks,
} from "../src/init/settings.ts";
import {
  absentRepos,
  installCommands,
  installSentence,
  summaryLine,
  nextBlock,
  writesLine,
  parseArgs,
  init,
  initWith,
  namedButExcluded,
  chooseEditors,
  CLOUD_NEXT_TITLE,
  cloudNextLines,
  ASK_YOUR_AGENT_PROMPT,
  type InitOptions,
} from "../src/init/run.ts";
import { inspectTaskSkills, removeTaskSkills, taskSkillPaths, taskSkillWarnings, writeTaskSkills } from "../src/init/task-skills.ts";
import { checkTaskSkills } from "../src/init/doctor.ts";
import { writeIfChanged } from "../src/init/files.ts";
import { WORKSPACE_FILE } from "../src/init/workspace-file.ts";
import { CODEX_HOOKS_FILE } from "../src/init/codex.ts";
import { TEMPLATE_PATHS } from "../src/templates.ts";
import {
  BEAT_MS,
  downloadHostedIndex,
  downloadProgress,
  hostedReport,
  localIndexState,
  REREADING,
  STEP_LABEL,
} from "../src/init/hosted.ts";
import type { MissingCommit, NativeRun } from "../src/init/hosted.ts";
import type { RunningScan, StatusResult, StatusService } from "../src/contract.ts";
import {
  DOCS,
  interactiveOutput,
  plainOutput,
  PromptCancelled,
  wrapped,
  type InitOutput,
} from "../src/init/output.ts";
import { PassThrough, Writable } from "node:stream";
import type { ResolvedRepos } from "../src/auth/read.ts";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

/**
 * A workspace whose derived proposal carries a config to write.
 *
 * A fixture that proposes nothing (`config: null`) cannot tell "init no longer
 * writes carrick.json" from "there was nothing to write", so the pinned-set
 * test uses this one: sixteen workspace members, three of them applications
 * and thirteen libraries, which is the shape a real monorepo's first run
 * derives.
 */
function monorepoProposal(workspace: string): Record<string, unknown> {
  const apps = ["gateway", "worker", "web"];
  const libraries = Array.from({ length: 13 }, (_, index) => `lib-${index + 1}`);
  return {
    schema: "carrick.derive/0",
    workspace,
    repos_detected_by: "single repository",
    repos_added: [],
    repos_excluded: [],
    missing: [],
    parent_proposal: null,
    repos: [
      {
        path: workspace,
        reason: "deno workspace",
        services: [...apps, ...libraries].map((name) => ({
          serviceName: name,
          directory: apps.includes(name) ? `apps/${name}` : `packages/${name}`,
        })),
        config: {
          services: [...apps, ...libraries].map((name) => ({
            name,
            directory: apps.includes(name) ? `apps/${name}` : `packages/${name}`,
          })),
        },
        warnings: [],
        not_installed: [],
      },
    ],
  };
}

function executableInitFixture(
  projectSlug: string,
  // Whether this fixture's API has the project actions at all. "absent" is the
  // deployed server: it answers an action it has never heard of with the
  // credential-kind gate, which is a 403 (carrick#955).
  projectActions: "absent" | "deployed" = "absent",
  // What the scanner's `derive` answers. The default proposes no config; the
  // monorepo one proposes a sixteen-service document.
  derived: "no-config" | "monorepo" = "no-config",
  // How this clone names its origin, and what a fake `ssh` on PATH resolves a
  // host alias to. Both default to the ordinary case: a literal github.com
  // remote, which resolves nothing and spawns no ssh. `expectRepos` is what
  // the workspace read must ask for, so a run that loses an identity fails
  // here rather than passing quietly.
  clone: { origin?: string; sshHostname?: string; expectRepos?: string[] } = {},
  // What the workspace read says about the repos beyond their connection:
  // whether Carrick already holds an index for them, and which other repos
  // the project holds that this machine does not (carrick#993 rows 2 and 18).
  // An indexed workspace is also the one whose `refresh` and `status` the
  // fixture binary answers, because that is the branch that reads the hosted
  // index onto this machine (carrick#1020): `hostedState` is what the status
  // it writes reports, and `refreshFails` is the scanner refusing the read.
  //
  // `localIndex` is what `.carrick/` holds on this machine BEFORE the run,
  // which is what decides whether the local pass runs at all (carrick#1373).
  // It defaults to `absent`, the second-developer case: a fresh clone whose
  // `status` refuses until a pass has written an index.
  workspace: {
    indexed?: boolean;
    alsoInProject?: string[];
    hostedState?: string;
    /** The hosted row each service's status names, as `carrick status --json` writes it. */
    hosted?: { commit: string; source?: string; uploaded_by?: string };
    refreshFails?: boolean;
    localIndex?: "absent" | "stale" | "current";
  } = {},
): {
  root: string;
  repo: string;
  env: NodeJS.ProcessEnv;
  /** The actions this fixture's server was asked for, in order. */
  requests: () => string[];
  /** The scanner subcommands this run spawned, in order (carrick#1373). */
  commands: () => string[];
  cleanup: () => void;
} {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-init-cli-"));
  const repo = path.join(root, "repo");
  fs.mkdirSync(repo);
  execFileSync("git", ["init", "-q", repo]);
  execFileSync("git", ["-C", repo, "remote", "add", "origin", clone.origin ?? "git@github.com:acme/api.git"]);

  const native = path.join(root, "native.mjs");
  // Anything but `derive` — and, on an indexed workspace, the free `refresh`
  // and `status` that read the hosted index onto this machine — exits 2, so a
  // run that ends 0 is a run that never asked this binary to scan.
  //
  // Stateful in one way: `status` answers "no index here" until a `refresh`
  // has run, unless this fixture was built with one already on the machine.
  // That is what makes the command log below a proof rather than a count — a
  // run that skips the local pass and a run that spends 527 seconds on it
  // reach the same rows, and only the log tells them apart (carrick#1373).
  fs.writeFileSync(native, `#!/usr/bin/env node
import fs from "node:fs";
const argv = process.argv.slice(2);
const indexed = ${JSON.stringify(workspace.indexed === true)};
const localIndex = ${JSON.stringify(workspace.localIndex ?? "absent")};
const commands = ${JSON.stringify(path.join(root, "native.log"))};
const wrote = ${JSON.stringify(path.join(root, "refreshed"))};
const at = (flag) => argv[argv.indexOf(flag) + 1];
fs.appendFileSync(commands, argv.join(" ") + "\\n");
if (indexed && argv[0] === "refresh") {
  if (${JSON.stringify(workspace.refreshFails === true)}) {
    process.stderr.write("carrick refresh: api has no carrick.json\\n");
    process.exit(1);
  }
  fs.writeFileSync(wrote, "");
  process.stdout.write("indexed 1 repo(s) in 4.0s\\n");
  process.exit(0);
}
if (indexed && argv[0] === "status") {
  const here = localIndex !== "absent" || fs.existsSync(wrote);
  if (!here) {
    process.stdout.write(JSON.stringify({
      schema: "carrick.status/0", error: "not_indexed",
      message: "No index here yet. Run carrick index.", services: [],
    }));
    process.exit(1);
  }
  // Stale only until the pass has run: the drift is what the pass clears.
  const changed = localIndex === "stale" && !fs.existsSync(wrote) ? 2 : 0;
  // The drift sits on one of the two services, as a change to a file one
  // service's scan reads and the other's does not.
  const service = (name, state) => ({
    service: name, repo: at("--workspace"), index_commit: "abc1234",
    indexed_at: "2026-09-12T00:00:00Z", routes: 3, calls: 2,
    changed_since_index: name === "api" ? changed : 0,
    hosted_state: state,
    hosted: ${JSON.stringify(workspace.hosted ?? null)},
  });
  const state = ${JSON.stringify(workspace.hostedState ?? "enriched")};
  process.stdout.write(JSON.stringify({
    schema: "carrick.status/0", workspace: at("--workspace"),
    repos: [{ repo: at("--workspace"), name: "repo", changed_since_index: changed, outside_every_service: 0 }],
    services: [service("api", state), service("web", state)],
  }));
  process.exit(0);
}
if (argv[0] !== "derive") process.exit(2);
const workspace = argv[argv.indexOf("--workspace") + 1];
const monorepo = ${JSON.stringify(derived === "monorepo")};
const proposal = monorepo ? ${JSON.stringify(monorepoProposal("WORKSPACE"))} : {
  schema: "carrick.derive/0", workspace, repos_detected_by: "single repository",
  repos_added: [], repos_excluded: [], missing: [], parent_proposal: null,
  repos: [{ path: workspace, reason: "single repository", services: [{ serviceName: null }], config: null, warnings: [], not_installed: [] }],
};
process.stdout.write(JSON.stringify(proposal).replaceAll("WORKSPACE", workspace));
`);
  fs.chmodSync(native, 0o755);

  const mockHttp = path.join(root, "mock-http.mjs");
  fs.writeFileSync(mockHttp, `
import fs from "node:fs";
// Every action this run asked of the server, in order. What a run must NOT
// ask is as much of the contract as what it asks: a move issued before the
// proposal was answered is invisible in stdout and plain here (carrick#1338).
const log = ${JSON.stringify(path.join(root, "requests.log"))};
const created = new Set();
// The assignment this workspace currently holds, which \`assign-repos\` moves
// and \`resolve-repos\` then reads back: the CLI claims nothing it has not read.
let placed = ${JSON.stringify(projectSlug)};
globalThis.fetch = async (input, init) => {
  if (String(input) !== "https://api.carrick.tools/types/check-or-upload") throw new Error("unexpected URL");
  const body = JSON.parse(String(init.body));
  fs.appendFileSync(log, body.action + (Array.isArray(body.repos) ? " " + body.repos.join(",") : "") + "\\n");
  if (body.action === "list-projects" || body.action === "create-project" || body.action === "assign-repos") {
    if (${JSON.stringify(projectActions)} === "absent") {
      return Response.json({ error: "MCP keys cannot authenticate scan traffic." }, { status: 403 });
    }
    if (body.action === "create-project") {
      created.add(body.slug);
      return Response.json({
        schema: "carrick.create-project/0",
        project: { slug: body.slug, name: body.name, archived: false, repo_count: 0 },
      });
    }
    if (body.action === "assign-repos") {
      // The server's own precondition: a project it does not hold is a 409,
      // not a move.
      if (body.project !== "default" && !created.has(body.project)) {
        return Response.json(
          { error: \`there is no project "\${body.project}" in the acme workspace. Create it first.\`, code: "project_not_found" },
          { status: 409 },
        );
      }
      const moved = placed !== body.project;
      placed = body.project;
      return Response.json({
        schema: "carrick.assign-repos/0",
        project_slug: body.project,
        repos: body.repos.map((name) => ({ full_name: name, assigned: true, moved, project_slug: body.project, reason: null })),
      });
    }
    return Response.json({
      schema: "carrick.list-projects/0",
      projects: [
        { slug: "default", name: "Default", archived: false, repo_count: 1 },
        // A project whose display name is not its slug: the CLI named projects
        // by slug alone and the dashboard's picker names them by display name,
        // with nothing connecting the two (carrick#1338).
        { slug: "default-project", name: "Acme Default", archived: false, repo_count: 1 },
        ...[...created].map((slug) => ({ slug, name: slug, archived: false, repo_count: 0 })),
      ],
    });
  }
  if (body.action !== "resolve-repos" || JSON.stringify(body.repos) !== JSON.stringify(${JSON.stringify(clone.expectRepos ?? ["acme/api"])})) throw new Error("unexpected request");
  const services = ${JSON.stringify(workspace.indexed === true)}
    ? [{ service: "api", hash: "h", updated_at: "2026-09-12T00:00:00Z", scanner_version: "0.3.62" }]
    : [];
  return Response.json({
    schema: "carrick.resolve-repos/0",
    workspace: { slug: "acme", billing_tier: "free", installed: true },
    allowance_sentence: null,
    repos: body.repos.map((name) => ({ full_name: name, connected: true, project_id: "p1", project_slug: placed, services })),
    project_repos: [{ project_slug: placed, repos: [...body.repos, ...${JSON.stringify(workspace.alsoInProject ?? [])}] }],
  });
};
`);

  // A fake `ssh` for the alias cases: OpenSSH reads ~/.ssh/config through the
  // password database rather than $HOME, so a temporary config file would not
  // be read and this is the only way to state the machine.
  const binDirectory = fakeClaude(root);
  if (clone.sshHostname !== undefined) {
    const ssh = path.join(binDirectory, "ssh");
    fs.writeFileSync(ssh, `#!/bin/sh\necho "hostname ${clone.sshHostname}"\n`);
    fs.chmodSync(ssh, 0o755);
  }

  return {
    root,
    repo,
    requests: () =>
      fs.existsSync(path.join(root, "requests.log"))
        ? fs.readFileSync(path.join(root, "requests.log"), "utf8").split("\n").filter((line) => line !== "")
        : [],
    commands: () =>
      fs.existsSync(path.join(root, "native.log"))
        ? fs
          .readFileSync(path.join(root, "native.log"), "utf8")
          .split("\n")
          .filter((line) => line !== "")
          .map((line) => line.split(" ")[0] ?? "")
        : [],
    env: {
      ...process.env,
      CARRICK_BIN: native,
      CARRICK_TOKEN: "test-token",
      XDG_CONFIG_HOME: path.join(root, "config"),
      // No registry lookup and no detached child for a test that only wants to
      // watch `init` write files (src/update.ts).
      CARRICK_NO_UPDATE_CHECK: "1",
      // The MCP step configures the agent clients this machine has, and the
      // detection is each client's own directory under the home directory.
      // A test that did not state one would edit the developer's own clients.
      HOME: path.join(root, "home"),
      USERPROFILE: path.join(root, "home"),
      NODE_OPTIONS: `--import=${mockHttp}`,
      PATH: `${binDirectory}${path.delimiter}${process.env["PATH"] ?? ""}`,
    },
    cleanup: () => fs.rmSync(root, { recursive: true, force: true }),
  };
}

/**
 * Set these environment variables, and put back exactly what was there.
 *
 * Keys, never the object: `process.env = { ...copy }` swaps Node's live
 * environment for a plain object, after which `os.homedir()` still reads the
 * real one, and an in-process test that follows looks at the home directory
 * of a fixture that was deleted a test ago (carrick#1489, where the editor
 * question then offered a file from it).
 */
function withEnv(changes: Record<string, string | undefined>): () => void {
  const before = new Map(Object.keys(changes).map((key) => [key, process.env[key]]));
  const apply = (entries: Iterable<[string, string | undefined]>): void => {
    for (const [key, value] of entries) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  };
  apply(Object.entries(changes));
  return () => apply(before);
}

/**
 * A `claude` command first on PATH, so the MCP step is the same on every
 * machine: without it, a developer's own Claude Code would be driven with the
 * fixture's home directory, and CI (which has none) would take the other
 * branch. It holds no server, takes the add, and logs what it was asked.
 */
function fakeClaude(root: string): string {
  const bin = path.join(root, "bin");
  fs.mkdirSync(bin, { recursive: true });
  const claude = path.join(bin, "claude");
  fs.writeFileSync(
    claude,
    `#!/bin/sh\necho "$*" >> ${JSON.stringify(path.join(root, "claude.log"))}\nif [ "$1 $2" = "mcp get" ]; then exit 1; fi\nexit 0\n`,
  );
  fs.chmodSync(claude, 0o755);
  return bin;
}

// Nothing under .carrick is ever committed, whichever command created it, and
// init writes nothing else into the folder it sets up the directory in.
test("the index directory init creates ignores itself, and holds nothing else", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-init-"));
  try {
    ensureIndexDir(dir);
    assert.equal(fs.readFileSync(path.join(dir, ".carrick", ".gitignore"), "utf8").trimEnd().split("\n").at(-1), "*");
    assert.deepEqual(fs.readdirSync(dir).sort(), [".carrick"]);
    assert.deepEqual(fs.readdirSync(path.join(dir, ".carrick")).sort(), [".gitignore"]);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

// The derive that now precedes this is a local read of the folder: it names
// the repos there so they can be chosen between, and it spawns no network and
// writes nothing (carrick#1338).
test("unsigned init refuses GH_TOKEN before writing or requesting the network", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-init-"));
  const restoreEnv = withEnv({ XDG_CONFIG_HOME: dir, GH_TOKEN: "github-is-not-carrick", CARRICK_TOKEN: undefined });
  const fetchBefore = globalThis.fetch;
  try {
    globalThis.fetch = async () => { throw new Error("must not request network"); };
    assert.equal(await init(["--yes", dir]), 1);
    assert.deepEqual(fs.readdirSync(dir), []);
  } finally {
    restoreEnv();
    globalThis.fetch = fetchBefore;
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("the hook entries land beside everything else the settings file holds", () => {
  const existing = JSON.stringify(
    {
      permissions: { allow: ["Bash(ls:*)"] },
      hooks: {
        SessionStart: [{ hooks: [{ type: "command", command: "./my-own-start.sh" }] }],
        PreToolUse: [{ matcher: "Grep", hooks: [{ type: "command", command: "./my-own-gate.sh" }] }],
      },
    },
    null,
    2,
  );
  const merged = mergeCarrickHooks(existing);
  const written = JSON.parse(merged.body);
  assert.deepEqual(written.permissions, { allow: ["Bash(ls:*)"] });
  assert.equal(written.hooks.PreToolUse.length, 1);
  // The user's own session-start script survives, with ours after it.
  assert.equal(written.hooks.SessionStart.length, 2);
  assert.equal(written.hooks.SessionStart[0].hooks[0].command, "./my-own-start.sh");
  assert.equal(written.hooks.SessionStart[1].hooks[0].command, "carrick hook session-start");
  assert.equal(written.hooks.PostToolUse[0].hooks[0].command, "carrick hook post-edit");
  // The Read hook (carrick#2069) is its own group beside the Edit one.
  assert.equal(written.hooks.PostToolUse[0].matcher, "Write|Edit|MultiEdit");
  assert.equal(written.hooks.PostToolUse[1].matcher, "Read");
  assert.equal(written.hooks.PostToolUse[1].hooks[0].command, "carrick hook post-read");
  assert.equal(written.hooks.PostToolUse[1].hooks[0].timeout, 5);
  // The same answer for a file a shell command reads.
  assert.equal(written.hooks.PostToolUse[2].matcher, "Bash");
  assert.equal(written.hooks.PostToolUse[2].hooks[0].command, "carrick hook post-bash");
  // The end-of-task re-check wait (carrick#3198). Installed beside the other
  // two rather than by anything the user has to add.
  assert.equal(written.hooks.Stop[0].hooks[0].command, "carrick hook stop --wait-s 200");
  assert.equal(written.hooks.Stop[0].matcher, undefined, "a Stop hook has no matcher");
});

// carrick#2854: the hosted scaffold's hook pack wires the same session-start
// message init does, so a session got it twice. Init replaces the pack's
// session-start entry (the exact `$CLAUDE_PROJECT_DIR/.claude/session-start.sh`
// command the scaffold writes) and keeps the pack's prompt reminder and search
// nudge, which init has nothing in place of, and every entry of the user's.
test("init replaces the scaffold's session-start entry and keeps its nudges and the user's own", () => {
  const pack = (name: string) => ({ type: "command", command: `"$CLAUDE_PROJECT_DIR/.claude/${name}.sh"` });
  const scaffolded = JSON.stringify(
    {
      permissions: { allow: ["Bash(ls:*)"] },
      hooks: {
        SessionStart: [{ hooks: [pack("session-start")] }],
        UserPromptSubmit: [{ hooks: [pack("turn-reminder")] }],
        PreToolUse: [
          { matcher: "Grep", hooks: [pack("search-gate")] },
          { matcher: "Bash", hooks: [pack("search-gate")] },
          // The user's own hook, sharing nothing with the pack but the event.
          { matcher: "Bash", hooks: [{ type: "command", command: "./guard.sh" }] },
        ],
        // A different script in the same directory is the user's, not the pack's.
        Stop: [{ hooks: [{ type: "command", command: '"$CLAUDE_PROJECT_DIR/.claude/notify.sh"' }] }],
      },
    },
    null,
    2,
  );
  const merged = mergeCarrickHooks(scaffolded);
  const written = JSON.parse(merged.body);
  assert.deepEqual(written.permissions, { allow: ["Bash(ls:*)"] });
  assert.deepEqual(
    written.hooks.SessionStart.map((group: { hooks: Array<{ command: string }> }) => group.hooks[0]!.command),
    ["carrick hook session-start"],
    "one session-start message",
  );
  assert.deepEqual(written.hooks.UserPromptSubmit, [{ hooks: [pack("turn-reminder")] }]);
  assert.deepEqual(written.hooks.PreToolUse, [
    { matcher: "Grep", hooks: [pack("search-gate")] },
    { matcher: "Bash", hooks: [pack("search-gate")] },
    { matcher: "Bash", hooks: [{ type: "command", command: "./guard.sh" }] },
  ]);
  assert.equal(written.hooks.Stop.length, 2);
  assert.equal(written.hooks.Stop[0].hooks[0].command, '"$CLAUDE_PROJECT_DIR/.claude/notify.sh"');
  assert.equal(JSON.stringify(written).includes("session-start.sh"), false);
  // Settled: a second run changes nothing.
  assert.equal(mergeCarrickHooks(merged.body).changed, false);
  // `carrick remove` is not a replacement: the pack's entries stay for their owner.
  assert.equal(removeCarrickHooks(scaffolded).changed, false);
});

test("merging twice changes nothing the second time", () => {
  const once = mergeCarrickHooks(null);
  const twice = mergeCarrickHooks(once.body);
  assert.equal(twice.body, once.body);
  assert.equal(twice.changed, false);
  assert.equal(once.changed, true);
});

test("moving hooks between shared and local settings removes only Carrick entries", () => {
  const before = JSON.stringify({ permissions: { allow: ["Bash(ls:*)"] }, hooks: { PostToolUse: [{ hooks: [{ type: "command", command: "eslint --fix" }] }] } });
  const local = mergeCarrickHooks(before, '"/opt/my tools/carrick/bin/carrick.mjs"').body;
  const cleaned = JSON.parse(mergeCarrickHooks(local, null).body);
  assert.deepEqual(cleaned, JSON.parse(before));
  const shared = mergeCarrickHooks(null).body;
  assert.deepEqual(JSON.parse(mergeCarrickHooks(shared, null).body), { hooks: {} });
});

test("an entry of ours that has moved on is replaced, not duplicated", () => {
  const stale = JSON.stringify(
    {
      hooks: {
        PostToolUse: [
          {
            matcher: "Write|Edit",
            hooks: [
              { type: "command", command: "carrick hook post-edit --old-flag" },
              { type: "command", command: "eslint --fix" },
            ],
          },
        ],
      },
    },
    null,
    2,
  );
  const written = JSON.parse(mergeCarrickHooks(stale).body);
  const commands = written.hooks.PostToolUse.flatMap((group: { hooks: Array<{ command: string }> }) =>
    group.hooks.map((entry) => entry.command),
  );
  assert.deepEqual(commands, ["eslint --fix", "carrick hook post-edit", "carrick hook post-read", "carrick hook post-bash"]);
});

test("a settings file that is not JSON is reported, never overwritten", () => {
  assert.throws(() => mergeCarrickHooks("{ this is not json"));
});

test("the hooks it claims are the only commands it will remove", () => {
  for (const groups of Object.values(carrickHooks())) {
    for (const group of groups) {
      for (const entry of group.hooks) {
        assert.match(entry.command, /^carrick hook /);
      }
    }
  }
});

test("the hook command is the bare name only when the bare name resolves", () => {
  const global = hookCommand({ onPath: () => true });
  assert.deepEqual(global, { command: "carrick", bare: true });

  const npxOnly = hookCommand({ onPath: () => false, root: "/opt/carrick" });
  assert.equal(npxOnly.bare, false);
  assert.equal(npxOnly.command, ownEntryPoint("/opt/carrick"));
  assert.match(npxOnly.command, /bin\/carrick\.mjs$/);

  // A path with a space in it is a path a shell must be handed quoted.
  const spaced = hookCommand({ onPath: () => false, root: "/opt/my tools/carrick" });
  assert.equal(spaced.command, `"${ownEntryPoint("/opt/my tools/carrick")}"`);
});

test("an absolute hook command is written, and is still recognised as ours", () => {
  const entry = ownEntryPoint("/opt/carrick");
  const written = JSON.parse(mergeCarrickHooks(null, entry).body);
  assert.equal(written.hooks.PostToolUse[0].hooks[0].command, `${entry} hook post-edit`);
  assert.equal(written.hooks.SessionStart[0].hooks[0].command, `${entry} hook session-start`);

  // The same workspace, now with carrick installed globally: one entry, not two.
  const rewritten = JSON.parse(mergeCarrickHooks(JSON.stringify(written, null, 2)).body);
  assert.equal(rewritten.hooks.PostToolUse.length, 3);
  assert.equal(rewritten.hooks.PostToolUse[0].hooks.length, 1);
  assert.equal(rewritten.hooks.PostToolUse[0].hooks[0].command, "carrick hook post-edit");
  assert.equal(rewritten.hooks.SessionStart.length, 1);
});

test("a quoted absolute hook command is ours too, and a lookalike is not", () => {
  const quoted = `"/opt/my tools/carrick/bin/carrick.mjs" hook post-edit`;
  const stale = JSON.stringify(
    {
      hooks: {
        PostToolUse: [
          {
            matcher: "Write|Edit",
            hooks: [
              { type: "command", command: quoted },
              { type: "command", command: "carrickctl hook post-edit" },
            ],
          },
        ],
      },
    },
    null,
    2,
  );
  const written = JSON.parse(mergeCarrickHooks(stale).body);
  const commands = written.hooks.PostToolUse.flatMap(
    (group: { hooks: Array<{ command: string }> }) => group.hooks.map((entry) => entry.command),
  );
  assert.deepEqual(commands, [
    "carrickctl hook post-edit",
    "carrick hook post-edit",
    "carrick hook post-read",
    "carrick hook post-bash",
  ]);
});

// carrick#1331. A checkout with `core.autocrlf` rewrites every tracked file to
// CRLF on the way to disk, so a settings file and all eight skill bodies
// differ from the rendered LF text in every line and nothing else. Compared as
// bytes, that is ten files rewritten on every single `carrick init` — and each
// rewrite puts LF back, which git then shows as modified work in a repository
// somebody is using.
test("a CRLF checkout is not rewritten by a run that changes nothing", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-crlf-"));
  try {
    const target = path.join(dir, "settings.json");
    const body = mergeCarrickHooks(null, "carrick").body;
    fs.writeFileSync(target, body.replace(/\n/g, "\r\n"));
    const before = fs.readFileSync(target);

    assert.equal(writeIfChanged(target, body), "unchanged");
    assert.deepEqual(fs.readFileSync(target), before, "and the CRLF file is left in CRLF");

    // The merge decides for itself whether a file changed, so it has to
    // compare the same way or `carrick init` reports a write that did not
    // happen.
    assert.equal(mergeCarrickHooks(body.replace(/\n/g, "\r\n"), "carrick").changed, false);

    // A real change is still a change.
    assert.equal(writeIfChanged(target, `${body}\n`), "written");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("init reads its arguments", () => {
  const parsed = parseArgs(["-y", "--project", "payments", "--workspace", "/code"], "/tmp");
  assert.deepEqual(parsed, {
    workspace: "/code",
    project: "payments",
    repos: [],
    editors: [],
    installGlobal: false,
  });
  // A global install is a change to the machine, not to this workspace, so
  // `--yes` is not it (carrick#1372).
  assert.equal((parseArgs(["--yes"]) as InitOptions).installGlobal, false);
  assert.equal((parseArgs(["--install-global"]) as InitOptions).installGlobal, true);
  // The editors an entry may be written for, named the way init prints them.
  assert.deepEqual((parseArgs(["--mcp", "Cursor"]) as InitOptions).editors, ["Cursor"]);
  assert.deepEqual((parseArgs(["--mcp", "Cursor,Windsurf"]) as InitOptions).editors, [
    "Cursor",
    "Windsurf",
  ]);
  assert.equal(parseArgs(["--mcp"]), "--mcp needs an editor name");
  // Repeatable, and comma-separable: a folder of repos is named one way or the
  // other, and both are the same list (carrick#1338).
  assert.deepEqual((parseArgs(["--repo", "acme/api"]) as InitOptions).repos, ["acme/api"]);
  assert.deepEqual(
    (parseArgs(["--repo", "acme/api", "--repo", "acme/web"]) as InitOptions).repos,
    ["acme/api", "acme/web"],
  );
  assert.deepEqual((parseArgs(["--repo", "acme/api,acme/web"]) as InitOptions).repos, [
    "acme/api",
    "acme/web",
  ]);
  assert.deepEqual((parseArgs(["--repo", "acme/api", "--repo", "acme/api"]) as InitOptions).repos, [
    "acme/api",
  ]);
  assert.match(parseArgs(["--repo"]) as string, /needs an owner\/repo/);
  for (const name of ["acme", "acme/api/extra", "https://github.com/acme/api"]) {
    assert.match(parseArgs(["--repo", name]) as string, /invalid repo/);
  }
  assert.equal((parseArgs(["/code"], "/tmp") as { workspace: string }).workspace, "/code");
  assert.equal((parseArgs(["--project", "payments"]) as { project: string }).project, "payments");
  assert.match(parseArgs(["--project"]) as string, /needs a slug/);
  assert.equal((parseArgs(["--project", "default"]) as { project: string }).project, "default");
  for (const slug of ["UPPER", "ab", "double--hyphen", "trailing-"]) {
    assert.match(parseArgs(["--project", slug]) as string, /invalid project slug/);
  }
  assert.match(parseArgs(["--nope"]) as string, /unknown option/);
  const help = parseArgs(["--help"]) as string;
  assert.match(help, /--project SLUG/);
  // `--project` checks, it never moves: repos change project on the dashboard
  // (carrick#3361).
  assert.match(help.replace(/\s+/g, " "), /Repos change project on the dashboard, so a project they are not in stops the run/);
  assert.match(help.replace(/\s+/g, " "), /Carrick builds the index itself, in the cloud: init asks nothing after sign-in, runs no scan/);
});

// The native override is an executable shebang fixture, which Windows cannot launch.
const posixNativeFixture = { skip: process.platform === "win32" ? "native shebang fixture requires POSIX" : false };

// What the scanner says when it finds no repos is the whole diagnosis
// (carrick#975), so it has to arrive intact and prefixed once.
test("a failed derive reaches the caller whole, with the scanner's own prefix removed", posixNativeFixture, () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-derive-"));
  const previous = process.env["CARRICK_BIN"];
  try {
    const native = path.join(dir, "native.mjs");
    const said = "carrick derive: no repos in /code.\\nLooked for: carrick.json, ...\\nFound: none of those manifests in /code. Inside it: no directories.";
    fs.writeFileSync(native, `#!/usr/bin/env node\nprocess.stderr.write("${said}\\n");\nprocess.exit(1);\n`);
    fs.chmodSync(native, 0o755);
    process.env["CARRICK_BIN"] = native;
    assert.throws(
      () => deriveWorkspace(dir),
      (error: Error) => {
        assert.ok(error.message.startsWith("no repos in /code."), error.message);
        assert.doesNotMatch(error.message, /carrick derive:/);
        assert.match(error.message, /Looked for: carrick\.json/);
        assert.match(error.message, /Inside it: no directories\./);
        return true;
      },
    );
  } finally {
    if (previous === undefined) delete process.env["CARRICK_BIN"];
    else process.env["CARRICK_BIN"] = previous;
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// carrick#1489 review: `not_installed` ships with the binary that fills it,
// so a document without it is a mismatched install, refused as one rather
// than read as "nothing to install".
test("a derive document without not_installed is refused as a mismatched install", posixNativeFixture, () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-derive-"));
  const restoreEnv = withEnv({ CARRICK_BIN: path.join(dir, "native.mjs") });
  try {
    const document = {
      schema: "carrick.derive/0", workspace: dir, repos_detected_by: "single repository",
      repos_added: [], repos_excluded: [], missing: [], parent_proposal: null,
      repos: [{ path: dir, reason: "single repository", services: [{ serviceName: "api" }], config: null, warnings: [] }],
    };
    fs.writeFileSync(path.join(dir, "native.mjs"), `#!/usr/bin/env node\nprocess.stdout.write(${JSON.stringify(JSON.stringify(document))});\n`);
    fs.chmodSync(path.join(dir, "native.mjs"), 0o755);
    assert.throws(() => deriveWorkspace(dir), /in a shape this CLI cannot read/);
  } finally {
    restoreEnv();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// carrick#3361. A project is changed on the dashboard, never from here, so a
// `--project` the repo is not in stops the run before anything is written or
// asked of Carrick beyond the read.
test("the executable CLI refuses a --project that would move the repo, and writes nothing", posixNativeFixture, () => {
  const fixture = executableInitFixture("default-project", "deployed");
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 1, result.stdout);
    assert.deepEqual(fixture.requests(), ["resolve-repos acme/api"]);
    assert.match(result.stdout, /repos change system on the dashboard: https:\/\/app\.carrick\.tools\/w\/acme\/repos/);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".carrick")), false);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".claude")), false);
  } finally {
    fixture.cleanup();
  }
});

/**
 * A folder of sibling repos, which is the shape carrick#1338 was reported on.
 *
 * Two repos, one of them the one a reader would drop: a folder routinely holds
 * a repo that must not be scanned, and there was no step at which to say so.
 * The server answers every read; what it is ASKED is the assertion.
 */
function folderInitFixture(): {
  folder: string;
  api: string;
  web: string;
  env: NodeJS.ProcessEnv;
  requests: () => string[];
  cleanup: () => void;
} {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-init-folder-")));
  const folder = path.join(root, "folder");
  fs.mkdirSync(folder);
  const made: Record<string, string> = {};
  for (const name of ["api", "web"]) {
    const repo = path.join(folder, name);
    fs.mkdirSync(repo);
    execFileSync("git", ["init", "-q", repo]);
    execFileSync("git", ["-C", repo, "remote", "add", "origin", `git@github.com:acme/${name}.git`]);
    made[name] = repo;
  }
  const native = path.join(root, "native.mjs");
  fs.writeFileSync(native, `#!/usr/bin/env node
const argv = process.argv.slice(2);
if (argv[0] !== "derive") process.exit(2);
const workspace = argv[argv.indexOf("--workspace") + 1];
process.stdout.write(JSON.stringify({
  schema: "carrick.derive/0", workspace, repos_detected_by: "sibling repositories",
  repos_added: [], repos_excluded: [], missing: [], parent_proposal: null,
  repos: [
    { path: ${JSON.stringify(made["api"])}, reason: "single repository", services: [{ serviceName: "api" }], config: null, warnings: [], not_installed: [] },
    { path: ${JSON.stringify(made["web"])}, reason: "npm workspaces", services: [{ serviceName: "web" }, { serviceName: "admin" }], config: null, warnings: [], not_installed: [] },
  ],
}));
`);
  fs.chmodSync(native, 0o755);

  const mockHttp = path.join(root, "mock-http.mjs");
  fs.writeFileSync(mockHttp, `
import fs from "node:fs";
const log = ${JSON.stringify(path.join(root, "requests.log"))};
let placed = "default-project";
globalThis.fetch = async (input, init) => {
  if (String(input) !== "https://api.carrick.tools/types/check-or-upload") throw new Error("unexpected URL");
  const body = JSON.parse(String(init.body));
  fs.appendFileSync(log, body.action + (Array.isArray(body.repos) ? " " + body.repos.join(",") : "") + "\\n");
  if (body.action === "list-projects") {
    return Response.json({
      schema: "carrick.list-projects/0",
      projects: [
        { slug: "default-project", name: "Acme Default", archived: false, repo_count: 2 },
        { slug: "payments", name: "Payments", archived: false, repo_count: 0 },
      ],
    });
  }
  if (body.action === "assign-repos") {
    const moved = placed !== body.project;
    placed = body.project;
    return Response.json({
      schema: "carrick.assign-repos/0", project_slug: body.project,
      repos: body.repos.map((name) => ({ full_name: name, assigned: true, moved, project_slug: body.project, reason: null })),
    });
  }
  if (body.action !== "resolve-repos") throw new Error("unexpected action " + body.action);
  return Response.json({
    schema: "carrick.resolve-repos/0",
    workspace: { slug: "acme", billing_tier: "free", installed: true },
    allowance_sentence: null,
    repos: body.repos.map((name) => ({ full_name: name, connected: true, project_id: "p1", project_slug: placed, services: [] })),
    project_repos: [{ project_slug: placed, repos: body.repos }],
  });
};
`);

  return {
    folder,
    api: made["api"]!,
    web: made["web"]!,
    requests: () =>
      fs.existsSync(path.join(root, "requests.log"))
        ? fs.readFileSync(path.join(root, "requests.log"), "utf8").split("\n").filter((line) => line !== "")
        : [],
    env: {
      ...process.env,
      CARRICK_BIN: native,
      CARRICK_TOKEN: "test-token",
      XDG_CONFIG_HOME: path.join(root, "config"),
      HOME: path.join(root, "home"),
      USERPROFILE: path.join(root, "home"),
      NODE_OPTIONS: `--import=${mockHttp}`,
      PATH: `${fakeClaude(root)}${path.delimiter}${process.env["PATH"] ?? ""}`,
    },
    cleanup: () => fs.rmSync(root, { recursive: true, force: true }),
  };
}

// carrick#1338 item 4: a folder routinely holds a repo that must not be
// indexed, and `--repo` is how a run leaves it out.
test("a repo --repo leaves out is named in no request, written into nothing, and recorded as left out", posixNativeFixture, () => {
  const fixture = folderInitFixture();
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--repo", "acme/api", "--yes", fixture.folder],
      { cwd: fixture.folder, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);

    // Carrick was never told about it.
    for (const request of fixture.requests()) assert.doesNotMatch(request, /acme\/web/);

    // And nothing of ours is inside it. The hooks and the skills are the
    // workspace's, at the folder root, so what this pins is that the repo
    // itself was not written into.
    for (const entry of [".carrick", ".claude", ".agents"]) {
      assert.equal(fs.existsSync(path.join(fixture.web, entry)), false, entry);
    }
    assert.equal(fs.existsSync(path.join(fixture.folder, ".claude")), true);

    // The half that used to be missing (carrick#1344): the answer is written
    // down, in the file the scanner reads before it derives, scans or answers
    // an editor hook about a file. `Workspace::load` honours the list by
    // directory name and the read path behind `carrick check` asks the same
    // question of the same file, both proven in `src/local_mode`.
    const selection = JSON.parse(
      fs.readFileSync(path.join(fixture.folder, WORKSPACE_FILE), "utf8"),
    );
    assert.deepEqual(selection.exclude, ["web"]);
    // And recorded as ours, which is what `carrick remove` takes back. The
    // file is named in the list the question that writes it is about
    // (carrick#1489, carrick#1512), not on a line of its own afterwards.
    assert.deepEqual(selection.carrick, { exclude: ["web"] });
    assert.match(result.stdout.replace(/\n +/g, " "), /Add Carrick's hooks .*create \.carrick\/ and carrick-workspace\.json/);
    assert.doesNotMatch(result.stdout, /web excluded in/);
  } finally {
    fixture.cleanup();
  }
});

// carrick#1344. An excluded repo is invisible to the scanner, so a `--repo`
// naming it would reach `selectRepos` as a value matching nothing on disk —
// and that rule attaches an unmatched value to the one repo here with no
// GitHub identity (carrick#991). Covering a different repository silently is
// the one outcome this cannot have.
test("a repo the workspace file excludes is named, not quietly re-covered", posixNativeFixture, () => {
  const fixture = folderInitFixture();
  try {
    fs.writeFileSync(
      path.join(fixture.folder, WORKSPACE_FILE),
      `${JSON.stringify({ exclude: ["web"], carrick: { exclude: ["web"] } }, null, 2)}\n`,
    );
    const result = spawnSync(
      process.execPath,
      [
        path.join(packageRoot, "bin", "carrick.mjs"), "init",
        "--repo", "acme/web", "--project", "payments", "--yes", fixture.folder,
      ],
      { cwd: fixture.folder, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, /carrick-workspace\.json leaves web out of this workspace/);
    assert.match(result.stderr, /exclude list/);
    // Nothing was asked of Carrick and nothing was written: the refusal is
    // before the login, as every other refusal in this command is.
    assert.deepEqual(fixture.requests(), []);
    assert.equal(fs.existsSync(path.join(fixture.folder, ".carrick")), false);
  } finally {
    fixture.cleanup();
  }
});

test("the executable CLI accepts the named assignment on repeated init", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments");
  try {
    for (let run = 0; run < 2; run += 1) {
      const result = spawnSync(
        process.execPath,
        [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
        { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
      );
      assert.equal(result.status, 0, result.stderr);
      // The project is stated once, on the one line that says what is set up
      // (carrick#1026, carrick#1489), and the verdict line is gone.
      assert.match(result.stdout, /^◇ Set up in project payments: repo$/m);
      assert.doesNotMatch(result.stdout, /Verified 1 repo in project|Signed in as/);
      assert.doesNotMatch(result.stdout, /Create project "payments" if needed/);
      // Nothing tells anyone to write a config or start a scan (carrick#3361).
      assert.doesNotMatch(result.stdout, /carrick\.json|carrick index|proposal|scaffold|paste this/i);
      // `claude` is on PATH and has never run under this home, so there is
      // no ~/.claude: it is still set up, by its own command, and nothing
      // says no agent client was found (carrick#1489). The entry carries
      // this machine's install id, which the run has just minted
      // (carrick-cloud#890).
      const installId = fs.readFileSync(path.join(fixture.root, "home", ".carrick", "install-id"), "utf8").trim();
      assert.match(installId, /^[A-Za-z0-9_-]{8,64}$/);
      assert.equal(fs.existsSync(path.join(fixture.root, "home", ".claude")), false);
      assert.ok(
        fs.readFileSync(path.join(fixture.root, "claude.log"), "utf8").includes(
          `mcp add --scope user --transport http carrick https://api.carrick.tools/mcp --header X-Carrick-Install-Id: ${installId}`,
        ),
      );
      assert.match(result.stdout.replace(/\n +/g, " "), /add Carrick's MCP server to Claude Code/);
      assert.doesNotMatch(result.stdout, /No agent client/);
      if (process.platform !== "win32") {
        // Nobody else's to read: it is this machine's name, not a shared one.
        const mode = fs.statSync(path.join(fixture.root, "home", ".carrick", "install-id")).mode & 0o777;
        assert.equal(mode, 0o600);
      }
    }
  } finally {
    fixture.cleanup();
  }
});

// carrick#993 rows 2 and 18 and carrick#1020: what a second machine joining an
// indexed project is told, and what it ends up holding. The paid scan has
// already run in CI, the project holds repos this folder does not, and the
// hosted index is read onto this machine instead of being withheld from it.
test("the executable CLI reads the hosted index onto an indexed repo and names the rest of the project", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "no-config", {}, {
    indexed: true,
    alsoInProject: ["acme/web", "acme/worker"],
  });
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.ok(
      result.stdout.includes("Also in this project, not on this machine: acme/web, acme/worker."),
      result.stdout,
    );
    // What happened, not a prohibition: the sentence this replaces left the
    // machine with no index and told the reader not to run the only command
    // that would have built one (carrick#1020).
    assert.ok(
      result.stdout.includes("◇ Hosted index for 2 services read into .carrick/"),
      result.stdout,
    );
    // And the pass ran, because this clone had no index for it to skip.
    assert.deepEqual(fixture.commands(), ["derive", "status", "refresh", "status"]);
    assert.doesNotMatch(result.stdout, /already has a hosted index\. Do not run/);
    assert.doesNotMatch(result.stdout, /There is no index yet/);
    // And nothing tells the reader to run a pass by hand: the read above is
    // this command's (carrick#1008, cloud#832).
    assert.doesNotMatch(result.stdout, /carrick refresh|carrick index/);
    // The run closes on the new-session step and the first question, and on
    // no instruction to write a config (carrick#3286, carrick#3361).
    assert.ok(result.stdout.includes(CLOUD_NEXT_TITLE), result.stdout);
    assert.ok(result.stdout.includes(ASK_YOUR_AGENT_PROMPT), result.stdout);
    assert.doesNotMatch(result.stdout, /carrick\.json|proposal|scaffold|paste this/i);
    assert.doesNotMatch(result.stdout, /is connected and has no hosted index yet/);
  } finally {
    fixture.cleanup();
  }
});

// carrick#1373, end to end. `carrick index` writes an index for this tree, and
// the `carrick init` that follows used to spend the whole of that pass again —
// 527 seconds on a three-repo workspace, under a label reading "Downloading"
// and a `--help` saying it ran no analysis.
test("the executable CLI re-reads nothing when this machine already holds a current index", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "no-config", {}, {
    indexed: true,
    localIndex: "current",
  });
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    // The proof: the scanner was asked to read, and never to scan.
    assert.ok(!fixture.commands().includes("refresh"), fixture.commands().join(", "));
    assert.ok(
      result.stdout.includes("◇ .carrick/ already holds the hosted index for 2 services: nothing was re-read"),
      result.stdout,
    );
    // And nothing claims a read that did not happen.
    assert.doesNotMatch(result.stdout, /read into \.carrick/);
    assert.doesNotMatch(result.stdout, /re-reading your code/);
  } finally {
    fixture.cleanup();
  }
});

// The other side of the same rule: an index behind its tree is re-read, and
// the run says so before the wait rather than after it.
test("the executable CLI re-reads a workspace whose index is behind its tree, and says why", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "no-config", {}, {
    indexed: true,
    localIndex: "stale",
  });
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.ok(fixture.commands().includes("refresh"), fixture.commands().join(", "));
    assert.ok(
      result.stdout.includes("2 file(s) changed since it was built, re-reading your code"),
      result.stdout,
    );
    assert.ok(result.stdout.includes("Hosted index for 2 services read into .carrick/"), result.stdout);
  } finally {
    fixture.cleanup();
  }
});

// carrick#1012 item 1. `CACHE_VERSION` moves most weeks, so a hosted blob is
// behind the installed CLI far more often than the CLI is wrong, and the
// sentence that asked for a downgrade asked the reader to give up every fix
// since.
test("a hosted index older than this CLI is reported as such, and no downgrade is asked for", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "no-config", {}, {
    indexed: true,
    hostedState: "version_mismatch",
  });
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    // No command on this machine clears it: Carrick builds the index, and
    // the agent still reads it through the MCP server (carrick#3361).
    assert.ok(result.stdout.includes(`▲ ${BUILT_BY_ANOTHER_RELEASE}`), result.stdout);
    assert.doesNotMatch(result.stdout, /carrick index/);
    assert.doesNotMatch(result.stdout, /npm i -g carrick@/);
    // And no claim that the hosted rows arrived, because they did not.
    assert.doesNotMatch(result.stdout, /downloaded into \.carrick/);
  } finally {
    fixture.cleanup();
  }
});

// carrick#1574. A hosted commit this clone does not have: the run names the
// commit and the repo from the status answer's `hosted` row, and the two
// commands that bring the rows. The Rust side proves those two commands do
// (`hosted_enrichment_test`), and that `hosted.commit` is where the commit is.
test("a hosted commit this clone lacks names the commit, the repo, and git fetch then carrick refresh", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "no-config", {}, {
    indexed: true,
    hostedState: "commit_missing",
    hosted: { commit: "4f2a1c9000000000000000000000000000000000" },
  });
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.ok(
      result.stdout.includes(
        `▲ Hosted index not read. It was built at commit 4f2a1c9, which this clone does not have. Run \`git fetch\` in ${path.basename(fixture.repo)}, then \`carrick refresh\`. Until then .carrick/ has only this machine's scan of 2 services.`,
      ),
      result.stdout,
    );
  } finally {
    fixture.cleanup();
  }
});

// A read that fails is a sentence, not a crash and not a silence: the rest of
// the setup is written either way, and the reader is told there is nothing to
// answer from yet.
test("a hosted read that fails leaves the setup written and says what went wrong", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "no-config", {}, {
    indexed: true,
    refreshFails: true,
  });
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.ok(
      result.stdout.includes(
        "■ Hosted index could not be read into .carrick/: api has no carrick.json",
      ),
      result.stdout,
    );
    // A refusal does not end the run: the rest of the setup is written, and
    // the closing block is the one every other branch ends on, with the read
    // still owed.
    assert.ok(result.stdout.includes(CLOUD_NEXT_TITLE), result.stdout);
    assert.match(result.stdout, /`carrick refresh` reads it in once Carrick has built it/);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".claude")), true);
  } finally {
    fixture.cleanup();
  }
});

/** Every path a run created under the workspace, git's own directory aside. */
function pathsUnder(root: string): string[] {
  const found: string[] = [];
  const walk = (directory: string): void => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      if (entry.name === ".git") continue;
      const full = path.join(directory, entry.name);
      if (entry.isDirectory()) walk(full);
      else found.push(path.relative(root, full));
    }
  };
  walk(root);
  return found.sort();
}

// The pinned set (carrick#974). The first run leaves the repository as it
// found it apart from the ignored `.carrick` directory, the hook settings and
// the task skills the two harnesses read. No carrick.json and no proposal: the
// config is Carrick's (carrick#3361). A change to this set is a deliberate
// diff in this list.
test("a first init writes the ignored .carrick directory, the hook settings, the task skills and the AGENTS.md check section, and nothing else",posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "monorepo");
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    // The fixture binary exits 2 for every command but `derive`, so a run that
    // ends 0 is a run that asked it for no scan.
    assert.equal(result.status, 0, result.stderr);

    // The run reaches its closing line, the project and its repo.
    assert.match(result.stdout, /^◇ Set up in project payments: repo$/m);

    const onPath =
      spawnSync(process.platform === "win32" ? "where" : "which", ["carrick"], { stdio: "ignore" })
        .status === 0;
    assert.deepEqual(
      pathsUnder(fixture.repo),
      [
        path.join(".carrick", ".gitignore"),
        // Which build wrote the files below, so a hook running an older
        // `carrick` than this one says so (carrick#1372).
        path.join(".carrick", "cli-version"),
        path.join(".claude", onPath ? "settings.json" : "settings.local.json"),
        // The same hooks, for the other host (carrick#1335).
        CODEX_HOOKS_FILE,
        // The check section for agents (carrick#2828).
        "AGENTS.md",
        ...taskSkillPaths(),
      ].sort(),
    );
    assert.equal(
      fs.readFileSync(path.join(fixture.repo, "AGENTS.md"), "utf8").includes("Never leave a second copy of logic that"),
      true,
    );

    // And the set is ignored where it has to be: all git can see in the tree
    // after a first run is the three agent directories, which hold the settings,
    // the Codex hooks and the skills and are meant to be committed.
    assert.equal(
      execFileSync("git", ["-C", fixture.repo, "status", "--porcelain"], { encoding: "utf8" }),
      "?? .agents/\n?? .claude/\n?? .codex/\n?? AGENTS.md\n",
    );

    // This workspace read reports no services, so the run names the index
    // Carrick has yet to build and closes on the new-session block, with the
    // read and the Codex trust step still open. No "Restart Claude Code"
    // line: the block says a NEW agent session, which is the one that loads
    // the server (carrick#1512, carrick#3286).
    assert.ok(
      result.stdout.includes(
        "▲ Carrick has no index for acme/api yet. It builds it in the cloud, and https://app.carrick.tools/w/acme/repos shows where it is. Run carrick refresh here once it is ready.",
      ),
      result.stdout,
    );
    assert.ok(
      result.stdout.trimEnd().endsWith(
        [
          CLOUD_NEXT_TITLE,
          `  ${ASK_YOUR_AGENT_PROMPT}.`,
          "  ",
          "  The index is not on this machine yet: `carrick refresh` reads it in once Carrick has built it.",
          "  ",
          "  Codex: open `codex` here once and trust the project's hooks, or it skips them.",
        ].join("\n"),
      ),
      result.stdout,
    );
    assert.doesNotMatch(result.stdout, /Restart Claude Code|proposal|carrick\.json|carrick index/);
    // The quickstart carries what left the terminal, and none of it is printed.
    assert.doesNotMatch(result.stdout, /--plugin-dir|--install-extension|carrick templates workflow|docs\.carrick\.tools\/carrick-json/);
  } finally {
    fixture.cleanup();
  }
});

// Blueprint row N35 (carrick#3361): config comes only from the cloud. The
// workspace here DERIVES a config worth writing, which is the case the laptop
// lane used to save to `.carrick/proposal.json` for an agent to turn into a
// carrick.json. The replacement is that init writes neither, and tells nobody
// to write one.
test("init saves no derive proposal and no carrick.json even when the workspace derives a config", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "monorepo");
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".carrick", "proposal.json")), false);
    assert.equal(fs.existsSync(path.join(fixture.repo, "carrick.json")), false);
    assert.doesNotMatch(result.stdout, /proposal|carrick\.json|derives one for your agent to write/);
  } finally {
    fixture.cleanup();
  }
});

test("init writes the check block into an existing CLAUDE.md, and creates none where there is none (carrick#3515)", posixNativeFixture, () => {
  for (const existing of ["# Team rules\n", null]) {
    const fixture = executableInitFixture("payments", "absent", "monorepo");
    try {
      const claude = path.join(fixture.repo, "CLAUDE.md");
      if (existing !== null) fs.writeFileSync(claude, existing);
      const result = spawnSync(
        process.execPath,
        [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--yes", fixture.repo],
        { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
      );
      assert.equal(result.status, 0, result.stderr);
      if (existing === null) {
        assert.ok(!fs.existsSync(claude));
      } else {
        const body = fs.readFileSync(claude, "utf8");
        assert.ok(body.startsWith("# Team rules\n\n<!-- carrick:check start -->"), body);
        assert.equal(body.slice(existing.length + 1), fs.readFileSync(path.join(fixture.repo, "AGENTS.md"), "utf8"));
      }
    } finally {
      fixture.cleanup();
    }
  }
});

// carrick#991 / carrick#978. A machine signed in to two GitHub accounts writes
// its remotes through a per-account ssh alias, and the repository behind that
// alias is an ordinary GitHub repository. Reading the host literally dropped
// it, and a dropped repo took the project step, the connection check and the
// workspace read with it, in silence.
const ALIAS_REMOTE = "git@github.com-work:acme/api.git";

test("a GitHub remote written through an ssh host alias names the repository", () => {
  const asked: string[] = [];
  const identity = repoIdentity("/w/api", {
    remote: () => ALIAS_REMOTE,
    sshHostname: (host) => {
      asked.push(host);
      return "github.com";
    },
  });
  assert.deepEqual(identity, { path: "/w/api", name: "acme/api", remote: ALIAS_REMOTE, problem: null });
  // Resolved the way ssh resolves it, from the user's own configuration.
  assert.deepEqual(asked, ["github.com-work"]);

  // The ssh:// spelling of the same alias, which is a URL rather than an scp path.
  assert.equal(
    repoIdentity("/w/api", { remote: () => "ssh://git@github.com-work/acme/api.git", sshHostname: () => "github.com" }).name,
    "acme/api",
  );

  // A host that is already github.com costs no subprocess at all.
  const never: Parameters<typeof repoIdentity>[1] = {
    remote: () => "git@github.com:acme/api.git",
    sshHostname: () => {
      throw new Error("a literal github.com host must not be resolved");
    },
  };
  assert.equal(repoIdentity("/w/api", never).name, "acme/api");
  assert.equal(repoIdentity("/w/api", { ...never, remote: () => "https://github.com/acme/api.git" }).name, "acme/api");
  // An alias spelled without a user, which parses as a URL whose scheme is the host.
  assert.equal(
    repoIdentity("/w/api", { remote: () => "github.com-work:acme/api.git", sshHostname: () => "github.com" }).name,
    "acme/api",
  );
});

test("a repo that names no GitHub identity says which remote was read and why", () => {
  // `ssh -G` exits 0 for a host no configuration entry matches and echoes the
  // name back, so an unresolvable alias is a hostname that is not github.com,
  // never a non-zero status.
  const echoed = repoIdentity("/w/api", { remote: () => ALIAS_REMOTE, sshHostname: (host) => host });
  assert.equal(echoed.name, null);
  assert.equal(echoed.remote, ALIAS_REMOTE);
  assert.match(echoed.problem ?? "", /github\.com-work/);
  assert.match(echoed.problem ?? "", /not github\.com/);

  // No ssh on this machine, which is a different sentence from a wrong host.
  const absent = repoIdentity("/w/api", { remote: () => ALIAS_REMOTE, sshHostname: () => null });
  assert.equal(absent.name, null);
  assert.match(absent.problem ?? "", /ssh could not resolve/);

  // And the remotes that never were a GitHub repository.
  assert.equal(repoIdentity("/w/api", { remote: () => null, sshHostname: () => null }).problem, "it has no origin remote");
  const elsewhere = repoIdentity("/w/api", { remote: () => "https://example.com/acme/api.git", sshHostname: () => null });
  assert.equal(elsewhere.name, null);
  assert.match(elsewhere.problem ?? "", /not github\.com/);
  const local = repoIdentity("/w/api", { remote: () => "/srv/mirrors/api.git", sshHostname: () => null });
  assert.equal(local.name, null);
  assert.match(local.problem ?? "", /not a GitHub URL/);
});

test("the executable CLI resolves an ssh host alias and verifies the repo", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "no-config", {
    origin: ALIAS_REMOTE,
    sshHostname: "github.com",
  });
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /^◇ Set up in project payments: repo$/m);
    assert.doesNotMatch(result.stdout, /contributes no GitHub identity/);
  } finally {
    fixture.cleanup();
  }
});

test("the executable CLI names the repo it could not identify, and says what it skipped", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "no-config", {
    // An alias with no entry in this machine's ssh configuration: ssh answers
    // with the alias itself.
    origin: ALIAS_REMOTE,
    sshHostname: "github.com-work",
    // The workspace read asks for nothing, because nothing was identified.
    expectRepos: [],
  });
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    // A repo with no GitHub identity cannot be in a Carrick workspace, so
    // there is nothing to set up: the run stops before any request and writes
    // nothing (carrick#3361).
    assert.equal(result.status, 1, result.stderr);
    // The path, the remote as written, and what ssh made of it.
    assert.ok(
      result.stdout.includes(
        `${fixture.repo} contributes no GitHub identity: its origin ${ALIAS_REMOTE} names the host "github.com-work"`,
      ),
      result.stdout,
    );
    assert.match(result.stdout, /carrick init --repo owner\/repo/);
    assert.match(result.stdout, /HostName github\.com line in your ssh config/);
    assert.deepEqual(fixture.requests(), []);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".carrick")), false);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".claude")), false);
  } finally {
    fixture.cleanup();
  }
});

test("the executable CLI takes --repo for the identity a remote could not give", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments", "absent", "no-config", {
    origin: ALIAS_REMOTE,
    sshHostname: "github.com-work",
  });
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--repo", "acme/api", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.ok(result.stdout.includes(`◇ acme/api taken as the GitHub repository for ${fixture.repo}`), result.stdout);
    assert.match(result.stdout, /^◇ Set up in project payments: repo$/m);
    assert.doesNotMatch(result.stdout, /contributes no GitHub identity/);
  } finally {
    fixture.cleanup();
  }
});

// carrick#993 row 18. Carrick answers across every repo in a project, so a
// machine holding half of one gets half the answers, and nothing else in this
// command's output says which half it has.
test("the rest of the project is named, capped, and never guessed at", () => {
  const workspace = (project_repos: ResolvedRepos["project_repos"]): ResolvedRepos => ({
    schema: "carrick.resolve-repos/0",
    workspace: { slug: "acme", billing_tier: "free", installed: true },
    allowance_sentence: null,
    repos: [],
    project_repos,
  });

  assert.deepEqual(
    absentRepos(workspace([{ project_slug: "payments", repos: ["acme/api", "acme/web", "acme/worker"] }]), ["acme/api"], "payments"),
    ["Also in this project, not on this machine: acme/web, acme/worker."],
  );

  // Casing is GitHub's, not this machine's.
  assert.deepEqual(
    absentRepos(workspace([{ project_slug: "payments", repos: ["ACME/API"] }]), ["acme/api"], "payments"),
    [],
  );

  // With a project settled, the other projects are somebody else's business.
  assert.deepEqual(
    absentRepos(
      workspace([
        { project_slug: "payments", repos: ["acme/api", "acme/web"] },
        { project_slug: "search", repos: ["acme/index"] },
      ]),
      ["acme/api"],
      "payments",
    ),
    ["Also in this project, not on this machine: acme/web."],
  );

  // Without one, each project is named, because "this project" would not say
  // which.
  assert.deepEqual(
    absentRepos(
      workspace([
        { project_slug: "payments", repos: ["acme/api", "acme/web"] },
        { project_slug: "search", repos: ["acme/index"] },
      ]),
      ["acme/api"],
      null,
    ),
    [
      'Also in project payments, not on this machine: acme/web.',
      'Also in project search, not on this machine: acme/index.',
    ],
  );

  // A project can hold two hundred, so the tail is counted rather than printed.
  const many = Array.from({ length: 14 }, (_, index) => `acme/repo-${index}`);
  assert.deepEqual(absentRepos(workspace([{ project_slug: "payments", repos: many }]), [], "payments"), [
    `Also in this project, not on this machine: ${many.slice(0, 10).join(", ")} and 4 more.`,
  ]);
});

// carrick#1026. The rendering, stated once: every executable test above spawns
// the CLI with its stdout on a pipe, so what they pin is the plain rendering —
// and the plain rendering is what a CI job, an agent's shell and a `| tee` get.
// The three markers are `@clack/prompts`'s own, so the terminal and the pipe
// say the same words with the same symbols; only the gutter, the colour and the
// spinner are the terminal's.
// carrick#1338 item 3. Ctrl-C at "Which project should these repos be in?" was
// read as the prompt's own quiet answer — clack returns a cancel symbol, and
// both renderings turned it into "" or false — so the run carried on into the
// steps that question governed. A cancel ends the run.
test("a cancelled question is not an answer, in either rendering", async () => {
  const closing = new PassThrough();
  const plain = plainOutput(() => {}, { input: closing, output: new PassThrough() });
  const asked = plain.confirm("Install carrick on this machine?");
  closing.end();
  await assert.rejects(asked, PromptCancelled);

  const ending = new PassThrough();
  const typed = plainOutput(() => {}, { input: ending, output: new PassThrough() });
  const question = typed.ask("Type the folder's name to confirm");
  ending.end();
  await assert.rejects(question, PromptCancelled);

  // The library's own cancel, which is the one the report was made on: clack
  // answers Ctrl-C with a symbol rather than an error.
  const keys = new PassThrough();
  const rendered = new PassThrough();
  rendered.resume();
  const interactive = interactiveOutput(rendered, keys);
  // One at a time: two prompts reading one stream would let a single Ctrl-C
  // stand in for both, and each has its own cancel to answer.
  for (const prompt of [
    () => interactive.confirm("Install carrick on this machine?"),
    () => interactive.ask("Type the folder's name to confirm"),
  ]) {
    const pending = prompt();
    setImmediate(() => keys.write("\u0003"));
    await assert.rejects(pending, PromptCancelled);
  }
});

/** An `InitOutput` a test can be: every line kept, every question answered. */
function recordingOutput(
  overrides: Partial<InitOutput> = {},
): InitOutput & { lines: string[] } {
  const lines: string[] = [];
  const keep = (marker: string) => (text: string) => void lines.push(`${marker} ${text}`);
  return {
    lines,
    done: keep("◇"),
    warn: keep("▲"),
    refuse: keep("■"),
    say: keep(""),
    intro: keep(""),
    outro: keep(""),
    note: (title, body) => void lines.push([title, ...body].join("\n")),
    step: async (_label, work, report) => {
      const value = await work(() => {});
      lines.push(report(value).text);
      return value;
    },
    confirm: async () => true,
    ask: async () => "",
    accent: (text) => text,
    quiet: false,
    ...overrides,
  };
}

test("the plain rendering is one line per thing, with no colour and no box", () => {
  const written: string[] = [];
  const out = plainOutput((text) => void written.push(text));
  out.done("Repo acme/api connected");
  out.warn("Hosted index is older than this CLI");
  out.refuse("Hosted index could not be read into .carrick/: no reason");
  out.say("Connect repositories in your browser: https://app.carrick.tools/repos");
  out.note("Next: ask your agent", ["Use Carrick to list the services in this project."]);
  assert.deepEqual(written.join("").split("\n"), [
    "◇ Repo acme/api connected",
    "▲ Hosted index is older than this CLI",
    "■ Hosted index could not be read into .carrick/: no reason",
    "Connect repositories in your browser: https://app.carrick.tools/repos",
    "",
    "Next: ask your agent",
    "  Use Carrick to list the services in this project.",
    "",
    "",
  ]);
  // No ANSI anywhere: a captured log is read by a person or an agent, and an
  // escape sequence in it is noise in both cases.
  assert.doesNotMatch(written.join(""), /\[/);
});

// carrick#1032. A step is ONE line, whichever rendering is in play, and the
// line is the one its work reported: the label the step started with is
// scaffolding, and printing it as well spent two lines on one event.
test("a step prints its work's report, and never the label it started with", async () => {
  const written: string[] = [];
  const out = plainOutput((text) => void written.push(text));
  const value = await out.step("Reading the hosted index into .carrick/", async () => 3, (count) => ({
    kind: "warn",
    text: `.carrick/ holds ${count} services as this machine read them`,
  }));
  assert.equal(value, 3);
  assert.deepEqual(written, ["▲ .carrick/ holds 3 services as this machine read them\n"]);
});

// The interactive rendering, which no other test can see: every executable
// test spawns the CLI with its stdout on a pipe, so all of them pin the plain
// one. clack draws into the stream it is handed, so handing it one is the
// whole of the harness (carrick#1032).
test("the interactive step stops the spinner on the marker its work earned", async () => {
  const drawn: string[] = [];
  const stream = new Writable({
    write(chunk, _encoding, done) {
      drawn.push(chunk.toString());
      done();
    },
  });
  const out = interactiveOutput(stream);
  await out.step("Reading the hosted index into .carrick/", async () => "local_only", () =>
    hostedReport({ kind: "local_only", services: 3, state: "read_failed", reread: true }),
  );
  await out.step("Reading the hosted index into .carrick/", async () => "downloaded", () =>
    hostedReport({ kind: "downloaded", services: 2, reread: true, readFromHosted: [] }),
  );
  // Rendered, then read as text: the colours and the cursor are the
  // terminal's business and the markers are the contract.
  const plain = drawn.join("").replace(/\[[0-9;?]*[A-Za-z]/g, "");
  const lines = plain
    .split("\n")
    .map((line) => line.replace(/\s+/g, " ").trim())
    .filter((line) => line.length > 0 && line !== "│");
  assert.deepEqual(lines, [
    "▲ The hosted rows could not be replayed onto this checkout. Run `carrick status` to see why. Until then .carrick/ has only this machine's scan of 3 services.",
    "◇ Hosted index for 2 services read into .carrick/",
  ]);
  // The label is the spinner's while it spins, and nothing once it stops.
  assert.doesNotMatch(plain.split("\n").filter((line) => /[◇▲■]/.test(line)).join("\n"), /Reading the hosted index/);
});

// carrick#1489: the connect wait is a step, and Ctrl-C there means "stop
// waiting", not "stop the run". clack's spinner answers SIGINT by stopping
// itself, so the work's report has to be printed some other way or the
// "Stopped waiting" line is lost.
test("a step stopped by Ctrl-C still prints its work's report", async () => {
  const drawn: string[] = [];
  const stream = new Writable({
    write(chunk, _encoding, done) {
      drawn.push(chunk.toString());
      done();
    },
  });
  const out = interactiveOutput(stream);
  await out.step(
    "Waiting for GitHub: 0 of 1 repo connected",
    async () => {
      process.emit("SIGINT");
      return "stopped";
    },
    () => ({ kind: "warn", text: "Stopped waiting for repository connections; continuing local setup." }),
  );
  const plain = drawn.join("").replace(/\[[0-9;?]*[A-Za-z]/g, "");
  assert.match(plain, /▲\s+Stopped waiting for repository connections; continuing local setup\./);
});

// The four index states, as lines rather than as tags. Each one says what this
// machine now holds; the two with something to do about it carry the marker
// that says so, and the one that produced nothing is a refusal (carrick#1020,
// carrick#1026).
test("every hosted outcome is one line, and only an actionable one is a warning", () => {
  assert.deepEqual(hostedReport({ kind: "downloaded", services: 2, reread: true, readFromHosted: [] }), {
    kind: "done",
    text: "Hosted index for 2 services read into .carrick/",
  });
  assert.deepEqual(
    hostedReport({ kind: "downloaded", services: 1, reread: true, readFromHosted: [] }).text,
    "Hosted index for 1 service read into .carrick/",
  );
  // With the time the step took, which is what a wait of minutes ends on.
  assert.equal(
    hostedReport({ kind: "downloaded", services: 15, reread: true, readFromHosted: [] }, 184).text,
    "Hosted index for 15 services read into .carrick/ in 3m4s",
  );
  // And the same rows, on a run that spent none of those minutes: the reader
  // is told which of the two they got (carrick#1373).
  const kept = hostedReport({ kind: "downloaded", services: 15, reread: false, readFromHosted: [] }, 0.4);
  assert.equal(kept.kind, "done");
  assert.equal(kept.text, ".carrick/ already holds the hosted index for 15 services: nothing was re-read");
  assert.doesNotMatch(kept.text, /in 0/);
  const older = hostedReport({ kind: "version_mismatch", services: 2, reread: true });
  assert.equal(older.kind, "warn");
  assert.equal(older.text, BUILT_BY_ANOTHER_RELEASE);
  // And never a downgrade: `CACHE_VERSION` moves most weeks (carrick#1012).
  assert.doesNotMatch(older.text, /npm i -g carrick@/);
  // The failure first, then the commands that move it, then what .carrick/
  // holds meanwhile. `git fetch` alone moves nothing: the next init finds the
  // same record, so the refresh is named with it, and the repo is named
  // because a folder of repos is not itself one (carrick#1574).
  const missing = (entries: MissingCommit[], reread = true) =>
    hostedReport({ kind: "local_only", services: entries.length === 1 ? 1 : 3, state: "commit_missing", reread, missing: entries });
  const local = missing([{ repo: "orders", commit: "abc1234", laptop: null }]);
  assert.equal(local.kind, "warn");
  assert.equal(
    local.text,
    "Hosted index not read. It was built at commit abc1234, which this clone does not have. Run `git fetch` in orders, then `carrick refresh`. Until then .carrick/ has only this machine's scan of 1 service.",
  );
  assert.equal(
    missing([{ repo: "orders", commit: "abc1234", laptop: null }], false).text,
    "Hosted index not read. It was built at commit abc1234, which this clone does not have. Run `git fetch` in orders, then `carrick refresh`. Until then .carrick/ has only this machine's scan of 1 service; nothing was re-read.",
  );
  // A laptop row can be at a commit nobody pushed, and a fetch finds nothing
  // until it is: the sentence says so rather than sending the reader to fetch
  // for ever.
  assert.equal(
    missing([{ repo: "orders", commit: "abc1234", laptop: "ihor" }]).text,
    "Hosted index not read. It was built by a laptop scan from @ihor at commit abc1234, which this clone does not have. Once that commit is pushed, run `git fetch` in orders, then `carrick refresh`. Until then .carrick/ has only this machine's scan of 1 service.",
  );
  assert.equal(
    missing([{ repo: "orders", commit: "abc1234", laptop: "" }]).text,
    "Hosted index not read. It was built by a laptop scan at commit abc1234, which this clone does not have. Once that commit is pushed, run `git fetch` in orders, then `carrick refresh`. Until then .carrick/ has only this machine's scan of 1 service.",
  );
  assert.equal(
    missing([
      { repo: "orders", commit: "abc1234", laptop: null },
      { repo: "billing", commit: "def5678", laptop: null },
    ]).text,
    "Hosted index not read. It was built at commits these clones do not have (orders at abc1234, billing at def5678). Run `git fetch` in each, then `carrick refresh`. Until then .carrick/ has only this machine's scan of 3 services.",
  );
  // The other states keep their reason, in the same order, and a state
  // with a command attached names it.
  assert.equal(
    hostedReport({ kind: "local_only", services: 1, state: "read_failed", reread: true }).text,
    "The hosted rows could not be replayed onto this checkout. Run `carrick status` to see why. Until then .carrick/ has only this machine's scan of 1 service.",
  );
  assert.equal(
    hostedReport({ kind: "local_only", services: 2, state: "not_connected", reread: true }).text,
    "These repos are not connected to a Carrick project. For now .carrick/ has only this machine's scan of 2 services.",
  );
  // No state's tag reaches the terminal.
  assert.equal(
    hostedReport({ kind: "local_only", services: 1, state: "unknown", reread: true }).text,
    "The hosted rows were not replayed. For now .carrick/ has only this machine's scan of 1 service.",
  );
  // The state this run can cause itself: a repo it just connected under a name
  // the scanner cannot read back off the git remote (carrick#1056). The clause
  // has to name the remote as the reason, not the connection.
  const unnamed = hostedReport({ kind: "local_only", services: 1, state: "remote_unnamed", reread: true });
  assert.match(unnamed.text, /git remote here names no owner\/repo/);
  assert.doesNotMatch(unnamed.text, /not connected/);
  // A scan of this workspace is already running, so this one left it alone.
  const scanning = hostedReport({ kind: "scanning" });
  assert.equal(scanning.kind, "warn");
  assert.match(scanning.text, /nothing was re-read/);
  assert.match(scanning.text, /carrick status/);
  const failed = hostedReport({ kind: "failed", problem: "api has no carrick.json" });
  assert.deepEqual(failed, {
    kind: "refuse",
    text: "Hosted index could not be read into .carrick/: api has no carrick.json",
  });
  // No branch forbids a command: the sentence that did left the reader with no
  // index and nothing that would build one (carrick#1020).
  for (const outcome of [older, local, failed, scanning]) {
    assert.doesNotMatch(outcome.text, /Do not run/);
  }
});

/** A status service row, with only the fields the freshness rule reads. */
function statusService(
  service: string,
  repo: string,
  extra: Partial<StatusService> = {},
): StatusService {
  return {
    service,
    repo,
    index_commit: "abc1234",
    routes: 3,
    calls: 2,
    changed_since_index: 0,
    hosted_state: "enriched",
    ...extra,
  };
}

/** A whole status answer, for one repo at `/code/api` unless told otherwise. */
function statusAnswer(extra: Partial<StatusResult> = {}): StatusResult {
  return {
    schema: "carrick.status/0",
    workspace: "/code",
    services: [statusService("api", "/code/api")],
    repos: [{ repo: "/code/api", name: "api", changed_since_index: 0, outside_every_service: 0 }],
    ...extra,
  };
}

// carrick#1373. The pass behind this step re-reads every file in the
// workspace, so the question it answers is not "is there an index" but "would
// a re-read write a different one". Each branch below is a case where it would
// — and the first one, a clean tree already indexed, is the case that cost 527
// seconds on a three-repo workspace for no row.
test("a clean tree already indexed needs no re-read, and each way of not being one does", () => {
  assert.deepEqual(localIndexState(statusAnswer(), ["/code/api"]), { kind: "current" });

  // Nothing to read at all: the refusal body, which `status` answers non-zero
  // with, and the no-answer case.
  const empty = statusAnswer({ services: [], repos: [], error: "not_indexed" });
  assert.deepEqual(localIndexState(empty, ["/code/api"]), {
    kind: "reread",
    reason: "there is no index here yet",
  });
  assert.equal(localIndexState(null, ["/code/api"]).kind, "reread");

  // Source that moved since the index was built. Counted from both places the
  // scanner puts it: what a service's own scan reads, and what belongs to no
  // service in the repo.
  assert.deepEqual(
    localIndexState(statusAnswer({ services: [statusService("api", "/code/api", { changed_since_index: 4 })] }), [
      "/code/api",
    ]),
    { kind: "reread", reason: "4 file(s) changed since it was built" },
  );
  assert.deepEqual(
    localIndexState(
      statusAnswer({ repos: [{ repo: "/code/api", name: "api", changed_since_index: 9, outside_every_service: 2 }] }),
      ["/code/api"],
    ),
    { kind: "reread", reason: "2 file(s) changed since it was built" },
  );

  // A repo this folder holds and the index does not, which no drift count can
  // report: the index has no rows for it to be stale.
  assert.deepEqual(localIndexState(statusAnswer(), ["/code/api", "/code/web"]), {
    kind: "reread",
    reason: "it covers 1 fewer repo(s) than this folder holds",
  });

  // A hosted state this run has just changed the conditions of: the index was
  // built by a machine that was not signed in, or before these repos were
  // connected, and `carrick init` has just done both.
  for (const state of ["not_signed_in", "not_connected", "remote_unnamed", "no_index_yet"] as const) {
    assert.deepEqual(
      localIndexState(statusAnswer({ services: [statusService("api", "/code/api", { hosted_state: state })] }), [
        "/code/api",
      ]),
      { kind: "reread", reason: "it holds no hosted rows yet" },
      state,
    );
  }
  // And one a second read answers exactly the same way: the hosted blob's own
  // condition, which nothing this run does moves (carrick#1024).
  for (const state of ["version_mismatch", "read_failed", "commit_missing"] as const) {
    assert.deepEqual(
      localIndexState(statusAnswer({ services: [statusService("api", "/code/api", { hosted_state: state })] }), [
        "/code/api",
      ]),
      { kind: "current" },
      state,
    );
  }

  // A scan is writing this index right now: a second pass over the same tree
  // is two scans contending, and the one that is running is the answer.
  const scan = (state: RunningScan["status"], pid = process.pid): RunningScan => ({
    scan_id: "s1",
    pid,
    status: state,
    started_at: "2026-09-22T00:00:00Z",
  });
  assert.deepEqual(
    localIndexState(statusAnswer({ services: [], running_scans: [scan("running")] }), ["/code/api"]),
    { kind: "scanning" },
  );
  // A `running` row whose process is gone. The record outlives the scan — it
  // is cleared by the next build, not by the scan ending — so an interrupted
  // `carrick index` would otherwise leave init skipping the re-read for ever.
  assert.deepEqual(
    localIndexState(
      statusAnswer({ services: [statusService("api", "/code/api", { changed_since_index: 3 })], running_scans: [scan("running", 2_147_483_646)] }),
      ["/code/api"],
    ),
    { kind: "reread", reason: "3 file(s) changed since it was built" },
  );
  // And the rows that sit in the same list without holding anything: the scan
  // that finished is cleared by the next build, not by finishing
  // (carrick#1007 item 4).
  for (const state of ["finished", "failed", "dispatched"] as const) {
    assert.deepEqual(
      localIndexState(statusAnswer({ running_scans: [scan(state)] }), ["/code/api"]),
      { kind: "current" },
      state,
    );
  }
});

/** A scanner stub that logs what it was asked to run. */
function recordingRun(answers: Record<string, { status?: number; stdout?: string; stderr?: string }>): {
  run: NativeRun;
  commands: string[];
} {
  const commands: string[] = [];
  return {
    commands,
    run: (args) => {
      commands.push(args[0] ?? "");
      const answer = answers[args[0] ?? ""] ?? { status: 2, stderr: "unexpected command" };
      return Promise.resolve({
        status: answer.status ?? 0,
        stdout: answer.stdout ?? "",
        stderr: answer.stderr ?? "",
      });
    },
  };
}

// The measurement behind carrick#1373: `refresh` is `build(.., Pass::Facts)`,
// a full local extraction and type-capture pass over every repo — 527 seconds
// on a three-repo workspace, and the same minutes again for every `carrick
// init` after a `carrick index`. The proof is the command log: no count and no
// sentence can tell a skipped pass from a fast one.
test("init runs no local pass when the index is already current, and still reports the hosted state", async () => {
  const { run, commands } = recordingRun({
    status: { stdout: JSON.stringify(statusAnswer({ services: [statusService("api", "/code/api")] })) },
  });
  const said: string[] = [];
  const outcome = await downloadHostedIndex("/code", run, ["/code/api"], (line) => void said.push(line));
  assert.deepEqual(commands, ["status"]);
  assert.deepEqual(outcome, { kind: "downloaded", services: 1, reread: false, readFromHosted: [] });
  // And nothing is said about re-reading a thing that was not re-read.
  assert.deepEqual(said, []);
});

// The other half: a workspace whose index is behind its tree still gets the
// pass, because that is the only thing that brings the rows up to date.
test("init runs the local pass when the index is stale, and says why before the wait", async () => {
  const stale = statusAnswer({ services: [statusService("api", "/code/api", { changed_since_index: 6 })] });
  const { run, commands } = recordingRun({
    status: { stdout: JSON.stringify(stale) },
    refresh: { stdout: "indexed 1 repo(s)" },
  });
  const said: string[] = [];
  const outcome = await downloadHostedIndex("/code", run, ["/code/api"], (line) => void said.push(line));
  // Status, then the pass, then status again: the outcome is read back from
  // the index rather than assumed from an exit code (carrick#1012).
  assert.deepEqual(commands, ["status", "refresh", "status"]);
  assert.deepEqual(outcome, { kind: "downloaded", services: 1, reread: true, readFromHosted: [] });
  assert.deepEqual(said, [`${STEP_LABEL}: 6 file(s) changed since it was built, ${REREADING}`]);
});

// A workspace with no index at all — the second developer on a team, which is
// the case this step was added for (carrick#1020) — and a pass that refuses.
test("init runs the local pass when there is no index, and a refusal is the scanner's own words", async () => {
  const { run, commands } = recordingRun({
    status: { status: 1, stdout: JSON.stringify({ schema: "carrick.status/0", error: "not_indexed", services: [] }) },
    refresh: { status: 1, stderr: "carrick refresh: api has no carrick.json\n" },
  });
  const outcome = await downloadHostedIndex("/code", run, ["/code/api"]);
  assert.deepEqual(commands, ["status", "refresh"]);
  assert.deepEqual(outcome, { kind: "failed", problem: "api has no carrick.json" });
});

// carrick#1489 part 5: the scanner states which services were read without
// their dependencies installed, and init names each install where it runs,
// from the workspace, then the read that picks the types up.
test("the installs a local read lacked are named where each runs, then the refresh", () => {
  const plan = (repos: Array<{ path: string; missing: Array<{ directory: string; command: string }> }>): WorkspaceProposal => ({
    schema: "carrick.derive/0",
    workspace: "/code",
    repos_detected_by: "test",
    repos_added: [],
    repos_excluded: [],
    missing: [],
    parent_proposal: null,
    repos: repos.map((repo) => ({
      path: repo.path,
      reason: "npm workspaces",
      services: [{ serviceName: "s" }],
      config: null,
      warnings: [],
      not_installed: repo.missing.map((row) => ({ service: "s", ...row })),
    })),
  });
  // A single repo: the command, with nowhere to say.
  assert.deepEqual(installCommands(plan([{ path: "/code", missing: [{ directory: ".", command: "npm install" }] }])), [
    "`npm install`",
  ]);
  // A folder: each repo by its directory, a nested install by its path, and
  // two services sharing one install root named once.
  const folder = plan([
    { path: "/code/acme-api", missing: [{ directory: ".", command: "npm install" }, { directory: ".", command: "npm install" }] },
    { path: "/code/acme-app", missing: [{ directory: "apps/web", command: "pnpm install" }] },
    { path: "/code/docs", missing: [] },
  ]);
  const commands = installCommands(folder);
  assert.deepEqual(commands, ["`npm install` in acme-api", "`pnpm install` in acme-app/apps/web"]);
  assert.equal(
    installSentence(commands),
    "Dependencies are not installed, so the index here was read without their types. Run `npm install` in acme-api and `pnpm install` in acme-app/apps/web, then carrick refresh.",
  );
  assert.equal(installSentence([]), null);
  // A repo read in from the hosted index carries its types already (carrick#2839).
  assert.deepEqual(installCommands(folder, ["/code/acme-api"]), ["`pnpm install` in acme-app/apps/web"]);
});

// carrick#1489 part 4: nine blocks closed a first run. What is set up is one
// line; what is left to do is a line each, and only when there is something.
test("what is set up is one line: the project and the repos in it", () => {
  // In folder order, and saying what it reports: a bare "Shop (shop): api"
  // left the reader to guess (carrick#1574).
  assert.equal(summaryLine("Shop (shop)", ["shop-app", "shop-api"]), "Set up in project Shop (shop): shop-api, shop-app");
  assert.equal(summaryLine(null, ["shop-app"]), "Set up with no project: shop-app");
  assert.equal(summaryLine(null, []), "Set up with no project");
  assert.equal(
    summaryLine("Shop (shop)", Array.from({ length: 12 }, (_, index) => `repo-${String(index).padStart(2, "0")}`)),
    "Set up in project Shop (shop): repo-00, repo-01, repo-02, repo-03, repo-04, repo-05, repo-06, repo-07, repo-08, repo-09 and 2 more",
  );
});
// What the step says while the download runs. Minutes of silence on a line
// reading "Reading the hosted index into .carrick/" is what the owner met on a
// folder of five repos; the scanner already states where it is, and the only
// thing missing was a reader for it (carrick#1365).
test("the download says how far through the workspace it is, and how long", () => {
  const said: string[] = [];
  let clock = 1000;
  const report = downloadProgress((text) => void said.push(text), () => clock);
  // Not a marker: nothing is drawn for the scanner's ordinary log lines.
  report.read("2026-09-20T18:00:00Z  INFO carrick: indexing 5 repos");
  assert.deepEqual(said, []);

  clock = 3500;
  report.read(
    '@carrick-progress {"service":"api","service_index":7,"service_total":15,"phase":"files","done":40,"total":120}',
  );
  assert.deepEqual(said, [`${STEP_LABEL}: ${REREADING}, 7 of 15 services, 2.5s`]);

  // The hosted half says its size once, as a notice, because it is two
  // requests for the whole workspace rather than a count that ticks.
  clock = 9000;
  report.read('@carrick-notice {"text":"hosted bytes 3.2 MB"}');
  clock = 15_000;
  report.read(
    '@carrick-progress {"service":"web","service_index":8,"service_total":15,"phase":"files","done":5,"total":90}',
  );
  assert.deepEqual(said.at(-1), `${STEP_LABEL}: ${REREADING}, 8 of 15 services, 3.2 MB, 14.0s`);

  // And never a time remaining: there is no measured rate to derive one from.
  for (const line of said) assert.doesNotMatch(line, /left|remaining|eta/i);
});

// carrick#1373: the markers arrive when the scanner finishes a service, and
// the hosted request in front of them is one call that runs for minutes on a
// large project. An agent harness reading a pipe cannot tell that wait from a
// hang, and it is the silence that ends the run — so the line is written off a
// clock as well as off the markers, and never twice inside one beat.
test("the download says where it is on a clock, not only when a marker arrives", () => {
  const said: string[] = [];
  let clock = 0;
  const report = downloadProgress((text) => void said.push(text), () => clock);

  // Nothing has moved and nothing has been said, and the beat says so anyway.
  clock = BEAT_MS;
  report.beat();
  assert.deepEqual(said, [`${STEP_LABEL}: ${REREADING}, ${(BEAT_MS / 1000).toFixed(1)}s`]);

  // Two beats inside one interval are one line: the cadence is the promise in
  // both directions.
  clock = BEAT_MS + 1;
  report.beat();
  report.read(
    '@carrick-progress {"service":"api","service_index":1,"service_total":9,"phase":"files","done":1,"total":9}',
  );
  assert.equal(said.length, 1, said.join("\n"));

  // And a beat a whole interval later says where the markers got it to.
  clock = BEAT_MS * 2 + 1;
  report.beat();
  assert.equal(said.length, 2, said.join("\n"));
  assert.match(said[1] as string, /1 of 9 services/);
});

// The other half of the same defect: the plain rendering used to drop every
// progress line, so the cadence above reached a terminal and nothing else.
test("the plain rendering writes what a running step says, not only its report", async () => {
  const written: string[] = [];
  const out = plainOutput((text) => void written.push(text));
  await out.step(
    STEP_LABEL,
    async (progress) => {
      progress(`${STEP_LABEL}: ${REREADING}, 5.0s`);
      progress(`${STEP_LABEL}: ${REREADING}, 2 of 9 services, 10.0s`);
      return "ok";
    },
    () => ({ kind: "done", text: "Hosted index for 9 services read into .carrick/" }),
  );
  assert.deepEqual(written, [
    `${STEP_LABEL}: ${REREADING}, 5.0s\n`,
    `${STEP_LABEL}: ${REREADING}, 2 of 9 services, 10.0s\n`,
    "◇ Hosted index for 9 services read into .carrick/\n",
  ]);
});

// A file under the home directory is outside the workspace, so it is written
// for an editor whose own command answers on this machine, or one `--mcp`
// names, and never for one this machine does not run (carrick#1365).
test("an editor file is written only for an editor found here, or one somebody named", () => {
  const offered = [
    { name: "Cursor", file: "/home/dev/.cursor/mcp.json", installed: true },
    { name: "Windsurf", file: "/home/dev/.codeium/windsurf/mcp_config.json", installed: false },
  ];
  assert.deepEqual(chooseEditors(offered, []), ["Cursor"]);
  assert.deepEqual(chooseEditors([], []), []);
  assert.deepEqual(chooseEditors([], ["Cursor"]), []);

  // The flag names them, in the casing init prints, once each.
  assert.deepEqual(chooseEditors(offered, ["cursor", "Cursor"]), ["Cursor"]);
  assert.deepEqual(chooseEditors(offered, ["windsurf"]), ["Windsurf"]);
  assert.throws(
    () => chooseEditors(offered, ["Zed"]),
    /--mcp Zed names no editor configured on this machine\. The ones here are: Cursor, Windsurf\./,
  );

  // The files are named by path in the list printed before they are written
  // (carrick#1489), the home directory as ~, and as what happens to them:
  // every writer merges, and a list of folders read as though it replaced
  // them (carrick#1512).
  assert.equal(
    writesLine({ workspaceFile: false, editorFiles: [], claudeCode: true, folder: null, home: "/home/dev" }),
    "Add Carrick's hooks and skills to .claude/, .agents/ and .codex/, create .carrick/, and add Carrick's MCP server to Claude Code",
  );
  assert.equal(
    writesLine({ workspaceFile: false, editorFiles: ["/home/dev/.cursor/mcp.json"], claudeCode: false, folder: null, home: "/home/dev" }),
    "Add Carrick's hooks and skills to .claude/, .agents/ and .codex/, create .carrick/, and add Carrick's MCP server to ~/.cursor/mcp.json",
  );
  assert.equal(
    writesLine({ workspaceFile: true, editorFiles: ["/home/dev/.cursor/mcp.json"], claudeCode: true, folder: "~/shop", home: "/home/dev" }),
    `Add Carrick's hooks and skills to .claude/, .agents/ and .codex/ in ~/shop and in each repo, create .carrick/ and ${WORKSPACE_FILE}, and add Carrick's MCP server to Claude Code and ~/.cursor/mcp.json`,
  );
  // A folder run writes into each repo too, so the line says where
  // (carrick#1512, option A as ruled).
  assert.equal(
    writesLine({ workspaceFile: false, editorFiles: [], claudeCode: true, folder: "~/shop", home: "/home/dev" }),
    "Add Carrick's hooks and skills to .claude/, .agents/ and .codex/ in ~/shop and in each repo, create .carrick/, and add Carrick's MCP server to Claude Code",
  );
  assert.equal(
    writesLine({ workspaceFile: false, editorFiles: [], claudeCode: false, folder: null, home: "/home/dev" }),
    "Add Carrick's hooks and skills to .claude/, .agents/ and .codex/, and create .carrick/",
  );
});

// The list of files printed before they are written, inside the gutter: a long
// line is broken between words rather than left for the terminal to wrap under
// the gutter (carrick#1512).
test("the files the run writes are one wrapped line under Next:", () => {
  const writes = "Add Carrick's hooks and skills to .claude/, .agents/ and .codex/, create .carrick/, and add Carrick's MCP server to Claude Code";
  assert.equal(
    nextBlock([writes]),
    [
      "Next:",
      "  Add Carrick's hooks and skills to .claude/, .agents/ and .codex/,",
      "  create .carrick/, and add Carrick's MCP server to Claude Code",
    ].join("\n"),
  );
  assert.ok(wrapped(writes).every((line) => line.length <= 68));
});

// carrick#1512: which repos beside this one are candidates for "the same
// system". The scanner counts a folder holding only `.git`; the question
// counts a folder holding a JavaScript or TypeScript manifest, one the
// folder's own workspace file has not left out, and never this repo itself.
test("a sibling is a JavaScript repo in the folder above, not left out and not this one", () => {
  // A git repository each, but for `/code/lib`, which holds a manifest and no
  // `.git`: a folder inside some other repository, or none (review R2).
  const files = new Set([
    "/code/api/package.json", "/code/api/.git",
    "/code/web/deno.json", "/code/web/.git",
    "/code/app/package.json", "/code/app/.git",
    "/code/old/package.json", "/code/old/.git",
    "/code/py/.git",
    "/code/lib/package.json",
  ]);
  const plan: WorkspaceProposal = {
    schema: "carrick.derive/0", workspace: "/code/app", repos_detected_by: "single repository", repos_added: [], repos_excluded: [], missing: [],
    parent_proposal: { directory: "/code", repos: ["/code/api", "/code/app", "/code/lib", "/code/py", "/code/web", "/code/old"] },
    repos: [],
  };
  assert.deepEqual(siblingRepos(plan, ["old"], (target) => files.has(target)), ["/code/api", "/code/web"]);
  // A repository this folder sits inside is not a sibling.
  assert.deepEqual(
    siblingRepos({ ...plan, parent_proposal: { directory: "/code", repos: ["/code"] } }, [], () => true),
    [],
  );
  assert.deepEqual(siblingRepos({ ...plan, parent_proposal: null }), []);
});

// carrick#2219, carrick#3361. The workspace has already answered everything a
// setup could ask: which repos, which project, and that nobody here runs a
// scan. So the run reads those answers, prints what it adds, installs what it
// always installs and reads the index in.

/** How Carrick holds one repo of the folder. */
type HeldAs = "indexed" | "connected" | "absent";

const NOT_IN_WORKSPACE = (repos: string, verb: string, pronoun: string): string =>
  `${repos} ${verb} not in your Carrick workspace acme, so this run leaves ${pronoun} out. Add repos at https://app.carrick.tools/w/acme/repos, then run carrick init again.`;
const NONE_IN_WORKSPACE =
  "None of the repos here is in your Carrick workspace acme. Add them at https://app.carrick.tools/w/acme/repos. If they are in another workspace, run carrick logout, then carrick init, and choose that workspace when you sign in.";
const NO_INDEX_YET =
  "Carrick has no index for acme/web yet. It builds it in the cloud, and https://app.carrick.tools/w/acme/repos shows where it is. Run carrick refresh here once it is ready.";
const BUILT_BY_ANOTHER_RELEASE =
  "Hosted index was built by a different Carrick release than this CLI, so it isn't downloaded here. Your agent still reads it through Carrick's MCP server.";
const READ_WITHOUT_TYPES =
  "Dependencies are not installed, so the index here was read without their types. Run `npm install` in api, then carrick refresh.";
const SYSTEM_ON_DASHBOARD =
  "On this workspace repos change system on the dashboard: https://app.carrick.tools/w/acme/repos. Run carrick init without --project.";
const READY =
  "The index is on this machine: `carrick check <file>` answers for a file, and your agent can ask who calls an operation.";

/**
 * A folder of repos in the `acme` workspace, with the server and the scanner
 * answered in this process.
 *
 * `repos` is the folder and how the workspace holds each repo of it, and it is
 * read at every request, so a test changes it between two runs. `builder` is
 * what the workspace read says under `index_builder`; null leaves the key out,
 * which is what a server older than the field answers.
 *
 * The fake scanner derives the folder as the real one does (honouring the
 * workspace file's `exclude`), and answers `status` with "no index here" until
 * a `refresh` has run: the command log is then what says the read-in ran.
 */
function cloudWorkspace(options: {
  repos: Record<string, HeldAs>;
  builder?: string | null;
  /** The project each repo is in, where it is not `shop`. */
  projects?: Record<string, string>;
  /** What `status` says of an indexed repo's hosted rows after the read. */
  hostedState?: string;
  /** Repos whose dependencies the scanner found missing. */
  notInstalled?: string[];
  /** Repos the scanner read in from the hosted index rather than scanning here. */
  readFromHosted?: string[];
  /** Editors with a configuration directory here, and whether each one's command answers. */
  editors?: Record<string, boolean>;
}): {
  root: string;
  folder: string;
  repo: (name: string) => string;
  repos: Record<string, HeldAs>;
  asked: string[];
  commands: () => string[];
  restore: () => void;
} {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-init-cloud-")));
  const folder = path.join(root, "shop");
  const held = { ...options.repos };
  for (const name of Object.keys(held)) {
    const dir = path.join(folder, name);
    fs.mkdirSync(dir, { recursive: true });
    execFileSync("git", ["init", "-q", dir]);
    execFileSync("git", ["-C", dir, "remote", "add", "origin", `git@github.com:acme/${name}.git`]);
    fs.writeFileSync(path.join(dir, "package.json"), "{}\n");
  }
  const states = path.join(root, "states.json");
  const writeStates = (): void => fs.writeFileSync(states, JSON.stringify(held));
  writeStates();
  const native = path.join(root, "native.mjs");
  fs.writeFileSync(native, `#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
const argv = process.argv.slice(2);
fs.appendFileSync(${JSON.stringify(path.join(root, "native.log"))}, argv[0] + "\\n");
const workspace = fs.realpathSync(argv[argv.indexOf("--workspace") + 1]);
const wrote = ${JSON.stringify(path.join(root, "refreshed"))};
const marked = (dir) => fs.existsSync(path.join(dir, "package.json"));
const children = (dir) => fs.readdirSync(dir, { withFileTypes: true })
  .filter((entry) => entry.isDirectory() && !entry.name.startsWith("."))
  .map((entry) => path.join(dir, entry.name)).filter(marked).sort();
let exclude = [];
try { exclude = JSON.parse(fs.readFileSync(path.join(workspace, "carrick-workspace.json"), "utf8")).exclude ?? []; } catch {}
const inside = children(workspace).filter((repo) => !exclude.includes(path.basename(repo)));
const repos = inside.length > 0 ? inside : [workspace];
if (argv[0] === "refresh") {
  fs.writeFileSync(wrote, "");
  process.stdout.write("indexed " + repos.length + " repo(s) in 4.0s\\n");
  process.exit(0);
}
if (argv[0] === "status") {
  if (!fs.existsSync(wrote)) {
    process.stdout.write(JSON.stringify({ schema: "carrick.status/0", error: "not_indexed", message: "No index here yet.", services: [] }));
    process.exit(1);
  }
  const held = JSON.parse(fs.readFileSync(${JSON.stringify(states)}, "utf8"));
  const state = (repo) => ({ indexed: ${JSON.stringify(options.hostedState ?? "enriched")}, connected: "no_index_yet", absent: "not_connected" })[held[path.basename(repo)]];
  process.stdout.write(JSON.stringify({
    schema: "carrick.status/0", workspace,
    repos: repos.map((repo) => ({ repo, name: path.basename(repo), changed_since_index: 0, outside_every_service: 0, ...(${JSON.stringify(options.readFromHosted ?? [])}.includes(path.basename(repo)) ? { read_from_hosted: true } : {}) })),
    services: repos.map((repo) => ({
      service: path.basename(repo), repo, index_commit: "abc1234", indexed_at: "2026-10-12T00:00:00Z",
      routes: 3, calls: 2, changed_since_index: 0, hosted_state: state(repo), hosted: null,
    })),
  }));
  process.exit(0);
}
if (argv[0] !== "derive") process.exit(2);
const entry = (repo) => ({
  path: repo, reason: "test", services: [{ serviceName: path.basename(repo) }], config: null, warnings: [],
  not_installed: ${JSON.stringify(options.notInstalled ?? [])}.includes(path.basename(repo)) ? [{ service: path.basename(repo), directory: ".", command: "npm install" }] : [],
});
const base = { schema: "carrick.derive/0", workspace, repos_added: [], repos_excluded: exclude, missing: [] };
process.stdout.write(JSON.stringify(inside.length > 0
  ? { ...base, repos_detected_by: "sibling repositories", parent_proposal: null, repos: inside.map(entry) }
  : { ...base, repos_detected_by: "single repository",
      parent_proposal: { directory: path.dirname(workspace), repos: children(path.dirname(workspace)) },
      repos: [entry(workspace)] }));
`);
  fs.chmodSync(native, 0o755);
  const bin = fakeClaude(root);
  // The fake scanner's interpreter, by itself.
  fs.symlinkSync(process.execPath, path.join(bin, "node"));
  const home = path.join(root, "home");
  const directories: Record<string, string> = { cursor: ".cursor", windsurf: path.join(".codeium", "windsurf") };
  for (const [editor, installed] of Object.entries(options.editors ?? {})) {
    fs.mkdirSync(path.join(home, directories[editor]!), { recursive: true });
    if (!installed) continue;
    fs.writeFileSync(path.join(bin, editor), "#!/bin/sh\nexit 0\n");
    fs.chmodSync(path.join(bin, editor), 0o755);
  }
  const restoreEnv = withEnv({
    CARRICK_BIN: native,
    CARRICK_TOKEN: "test-token",
    XDG_CONFIG_HOME: path.join(root, "config"),
    HOME: home,
    // Nothing of the developer's own machine: an editor command found there
    // would be an editor file written here, and a global `carrick` beside
    // their node would hide the one question a machine without it is asked.
    PATH: [bin, "/usr/bin", "/bin"].join(path.delimiter),
  });
  const fetchBefore = globalThis.fetch;
  const asked: string[] = [];
  const projectOf = (name: string): string => options.projects?.[name] ?? "shop";
  globalThis.fetch = (async (_input: string, init: { body: string }) => {
    const body = JSON.parse(String(init.body));
    asked.push(`${body.action}${Array.isArray(body.repos) ? ` ${body.repos.join(",")}` : ""}`);
    if (body.action === "list-projects") {
      return Response.json({
        schema: "carrick.list-projects/0",
        projects: [
          { slug: "shop", name: "Shop", archived: false, repo_count: 2 },
          { slug: "billing", name: "Billing", archived: false, repo_count: 1 },
        ],
      });
    }
    // Nothing else is a read: a create, an assignment or a move on this lane
    // is the failure these tests exist to catch.
    if (body.action !== "resolve-repos") throw new Error(`unexpected action ${body.action}`);
    const short = (fullName: string): string => fullName.slice("acme/".length);
    const connected = (body.repos as string[]).filter((name) => (held[short(name)] ?? "absent") !== "absent");
    const slugs = [...new Set(connected.map((name) => projectOf(short(name))))];
    return Response.json({
      schema: "carrick.resolve-repos/0",
      workspace: {
        slug: "acme",
        billing_tier: "paid",
        installed: true,
        ...(options.builder === null ? {} : { index_builder: options.builder ?? "cloud" }),
      },
      allowance_sentence: null,
      repos: (body.repos as string[]).map((name) =>
        connected.includes(name)
          ? {
              full_name: name,
              connected: true,
              project_id: `p-${projectOf(short(name))}`,
              project_slug: projectOf(short(name)),
              services:
                held[short(name)] === "indexed"
                  ? [{ service: short(name), hash: "h", updated_at: "2026-10-12T00:00:00Z", scanner_version: "0.3.113" }]
                  : [],
            }
          : { full_name: name, connected: false }),
      project_repos: slugs.map((slug) => ({ project_slug: slug, repos: connected.filter((name) => projectOf(short(name)) === slug) })),
    });
  }) as unknown as typeof fetch;
  return {
    root,
    folder,
    repo: (name) => path.join(folder, name),
    get repos() {
      return held;
    },
    set repos(next) {
      for (const [name, state] of Object.entries(next)) held[name] = state;
      writeStates();
    },
    asked,
    commands: () =>
      fs.existsSync(path.join(root, "native.log"))
        ? fs.readFileSync(path.join(root, "native.log"), "utf8").split("\n").filter((line) => line !== "")
        : [],
    restore: () => {
      restoreEnv();
      globalThis.fetch = fetchBefore;
      fs.rmSync(root, { recursive: true, force: true });
    },
  };
}

/** A terminal that fails the run if it is asked anything, and records what. */
function silentTerminal(): InitOutput & { lines: string[]; questions: string[] } {
  const questions: string[] = [];
  const asked = (question: string): never => {
    questions.push(question);
    throw new Error(`asked: ${question}`);
  };
  const out = recordingOutput({
    confirm: async (question) => asked(question),
    ask: async (question) => asked(question),
  });
  return Object.assign(out, { questions });
}

/** What a run printed, with this fixture's own paths and the clock taken out. */
function printed(out: { lines: string[] }, root: string): string {
  return out.lines.join("\n").split(root).join("ROOT").replace(/ in \d+(\.\d+s|m\d+s)/g, " in Ns");
}

// The fields init used to branch on are not read at all: whatever the server
// says about who builds the index or who the user is, the read is the same
// read (carrick#3361).
test("the workspace read takes any index_builder or role, or none, and keeps neither", async () => {
  const { resolveRepos } = await import("../src/auth/read.ts");
  const answer = (workspace: Record<string, unknown>): typeof fetch =>
    (async () =>
      Response.json({
        schema: "carrick.resolve-repos/0",
        workspace: { slug: "acme", billing_tier: "free", installed: true, ...workspace },
        allowance_sentence: null,
        repos: [],
        project_repos: [],
      })) as unknown as typeof fetch;
  for (const extra of [{}, { index_builder: "cloud" }, { index_builder: "laptop", role: "member" }, { index_builder: 7, role: "someday" }]) {
    const read = await resolveRepos("t", [], answer(extra));
    assert.deepEqual(read.workspace, { slug: "acme", billing_tier: "free", installed: true }, JSON.stringify(extra));
  }
});

test("the cloud lane's closing block is the prompt plus only the steps still open", () => {
  assert.equal(CLOUD_NEXT_TITLE, "Done. Open a new agent session in this folder, then ask it:");
  assert.deepEqual(cloudNextLines(false, false), [`${ASK_YOUR_AGENT_PROMPT}.`]);
  assert.deepEqual(cloudNextLines(true, true), [
    `${ASK_YOUR_AGENT_PROMPT}.`,
    "",
    "The index is not on this machine yet: `carrick refresh` reads it in once Carrick has built it.",
    "",
    "Codex: open `codex` here once and trust the project's hooks, or it skips them.",
  ]);
});

test("where Carrick builds the index, init asks nothing, installs everything and reads the index in", posixNativeFixture, async () => {
  // With a terminal and without one: neither is asked anything, and neither
  // needs --yes.
  for (const interactive of [true, false]) {
    const fixture = cloudWorkspace({ repos: { api: "indexed" } });
    const out = silentTerminal();
    try {
      assert.equal(await initWith([fixture.repo("api")], out, interactive), 0, out.lines.join("\n"));
      assert.deepEqual(out.questions, []);
      // Two reads, and nothing that changes Carrick.
      assert.deepEqual(fixture.asked, ["resolve-repos acme/api", "list-projects"]);
      // No scan: the local read, and the status either side of it.
      assert.deepEqual(fixture.commands(), ["derive", "status", "refresh", "status"]);
      const text = out.lines.join("\n");
      assert.match(text, /Next:\n {2}Add Carrick's hooks and skills to \.claude\/, \.agents\/ and \.codex\//);
      assert.ok(out.lines.includes("◇ Set up in project Shop (shop): api"), text);
      assert.match(text, /^Hosted index for 1 service read into \.carrick\/ in \d/m);
      assert.ok(out.lines.includes(`◇ ${READY}`), text);
      // The run closes on the new-session step, the prompt and, on a first
      // write of the Codex hooks, the trust step, said once and last
      // (carrick#3286); the laptop lane's paste-this block never appears.
      const closing = text.slice(text.indexOf(CLOUD_NEXT_TITLE));
      assert.ok(closing.includes(`${ASK_YOUR_AGENT_PROMPT}.`), closing);
      assert.match(closing, /Codex: open `codex` here once and trust the project's hooks, or it skips them\./);
      assert.doesNotMatch(closing, /Written here|so it loads the MCP server|socket events and queues/);
      assert.doesNotMatch(text, /choose to trust the project's hooks/);
      // The index is on this machine, so refresh is not offered.
      assert.doesNotMatch(closing, /carrick refresh/);
      // Nobody scaffolds here: no instruction for an agent, no config, no
      // workflow, no scan command.
      assert.doesNotMatch(text, /scaffold|carrick\.json|carrick index|workflow|paste this/i);
      const repo = fixture.repo("api");
      assert.equal(fs.existsSync(path.join(repo, "carrick.json")), false);
      assert.equal(fs.existsSync(path.join(repo, TEMPLATE_PATHS.workflow)), false);
      // Every install step ran: the hooks for both hosts, the skills under
      // both roots, and Claude Code's MCP server; and no proposal.
      assert.equal(fs.existsSync(path.join(repo, ".carrick", "proposal.json")), false);
      assert.equal(
        fs.existsSync(path.join(repo, ".claude", "settings.json")) || fs.existsSync(path.join(repo, ".claude", "settings.local.json")),
        true,
      );
      assert.equal(fs.existsSync(path.join(repo, CODEX_HOOKS_FILE)), true);
      for (const file of taskSkillPaths()) assert.equal(fs.existsSync(path.join(repo, file)), true, file);
      assert.match(fs.readFileSync(path.join(repo, taskSkillPaths()[0]!), "utf8"), /project: "shop"/);
      assert.match(fs.readFileSync(path.join(fixture.root, "claude.log"), "utf8"), /mcp add/);
    } finally {
      fixture.restore();
    }
  }
});

test("a folder is set up for the repos the workspace holds, and the rest are named and left out", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({ repos: { api: "indexed", web: "connected", notes: "absent", scratch: "indexed" } });
  try {
    // A repo somebody left out by hand, before any run.
    const workspaceFile = path.join(fixture.folder, WORKSPACE_FILE);
    fs.writeFileSync(workspaceFile, `${JSON.stringify({ exclude: ["scratch"] }, null, 2)}\n`);
    const first = silentTerminal();
    assert.equal(await initWith([fixture.folder], first, true), 0, first.lines.join("\n"));
    assert.deepEqual(first.questions, []);
    assert.deepEqual(fixture.asked, ["resolve-repos acme/api,acme/notes,acme/web", "list-projects"]);
    assert.ok(first.lines.includes(`▲ ${NOT_IN_WORKSPACE("acme/notes", "is", "it")}`), first.lines.join("\n"));
    assert.ok(first.lines.includes(`▲ ${NO_INDEX_YET}`), first.lines.join("\n"));
    assert.ok(first.lines.includes("◇ Set up in project Shop (shop): api, web"), first.lines.join("\n"));
    assert.ok(first.lines.includes(`◇ ${READY}`), first.lines.join("\n"));
    // A repo has no index yet: the closing block says refresh reads it in.
    assert.match(first.lines.join("\n"), /`carrick refresh` reads it in once Carrick has built it/);
    // The repo the workspace does not hold is out of the file every later
    // command reads, recorded as this command's, and holds nothing of ours.
    assert.deepEqual(JSON.parse(fs.readFileSync(workspaceFile, "utf8")), {
      exclude: ["scratch", "notes"],
      carrick: { exclude: ["notes"] },
    });
    assert.deepEqual(fs.readdirSync(fixture.repo("notes")).sort(), [".git", "package.json"]);
    for (const name of ["api", "web"]) assert.equal(fs.existsSync(path.join(fixture.repo(name), ".claude")), true, name);
    // One read-in for the folder, a repo with no index yet included.
    assert.deepEqual(fixture.commands(), ["derive", "status", "refresh", "status"]);

    // The repo joins the workspace, and the next run finds it without being
    // told: what init recorded was a result, not a choice.
    fixture.repos = { notes: "indexed" };
    fixture.asked.length = 0;
    const second = silentTerminal();
    assert.equal(await initWith([fixture.folder], second, true), 0, second.lines.join("\n"));
    assert.deepEqual(fixture.asked, [
      "resolve-repos acme/api,acme/web",
      "resolve-repos acme/api,acme/notes,acme/web",
      "list-projects",
    ]);
    assert.ok(second.lines.includes("◇ Set up in project Shop (shop): api, notes, web"), second.lines.join("\n"));
    assert.equal(fs.existsSync(path.join(fixture.repo("notes"), ".claude")), true);
    // The name a person wrote stays, is said, and was named in no request.
    assert.deepEqual(JSON.parse(fs.readFileSync(workspaceFile, "utf8")), { exclude: ["scratch"] });
    assert.ok(
      second.lines.includes(` ${WORKSPACE_FILE} leaves scratch out of this workspace, so nothing below covers it.`),
      second.lines.join("\n"),
    );
    assert.equal(second.lines.some((line) => line.includes("not in your Carrick workspace")), false);
    assert.deepEqual(fs.readdirSync(fixture.repo("scratch")).sort(), [".git", "package.json"]);
  } finally {
    fixture.restore();
  }
});

test("a folder with no repo in the workspace stops before it writes anything", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({ repos: { api: "absent", web: "absent" } });
  try {
    // What an earlier run on another lane recorded is put back as it was.
    const workspaceFile = path.join(fixture.folder, WORKSPACE_FILE);
    const recorded = `${JSON.stringify({ exclude: ["web"], carrick: { exclude: ["web"] } }, null, 2)}\n`;
    fs.writeFileSync(workspaceFile, recorded);
    const out = silentTerminal();
    assert.equal(await initWith([fixture.folder], out, true), 1, out.lines.join("\n"));
    assert.ok(out.lines.includes(`■ ${NONE_IN_WORKSPACE}`), out.lines.join("\n"));
    assert.deepEqual(fixture.asked, ["resolve-repos acme/api", "resolve-repos acme/api,acme/web"]);
    assert.equal(fs.readFileSync(workspaceFile, "utf8"), recorded);
    assert.deepEqual(fs.readdirSync(fixture.folder).sort(), ["api", WORKSPACE_FILE, "web"].sort());
    for (const name of ["api", "web"]) assert.deepEqual(fs.readdirSync(fixture.repo(name)).sort(), [".git", "package.json"]);
    assert.deepEqual(fixture.commands().filter((command) => command !== "derive"), []);
  } finally {
    fixture.restore();
  }
});

test("inside one repo of a folder, the run sets up that repo and says so", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({ repos: { api: "indexed", web: "indexed" } });
  try {
    const out = silentTerminal();
    assert.equal(await initWith([fixture.repo("api")], out, true), 0, out.lines.join("\n"));
    assert.ok(
      out.lines.includes(" Setting up api only. To set up several repos as one system, run carrick init in the folder that holds them."),
      out.lines.join("\n"),
    );
    // The repo beside it is named in no request and holds nothing of ours.
    assert.deepEqual(fixture.asked, ["resolve-repos acme/api", "list-projects"]);
    assert.deepEqual(fs.readdirSync(fixture.repo("web")).sort(), [".git", "package.json"]);
    assert.deepEqual(fs.readdirSync(fixture.folder).sort(), ["api", "web"]);
  } finally {
    fixture.restore();
  }
});

// carrick#2839. A repo read in from the hosted index carries the hosted scan's
// types, captured with dependencies installed, so the advice to install and
// refresh is false for it and costs a full local re-read.
test("on that workspace a repo read in from the hosted index is not told to install", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({
    repos: { api: "indexed", web: "indexed" },
    notInstalled: ["api", "web"],
    readFromHosted: ["api"],
  });
  try {
    const out = silentTerminal();
    assert.equal(await initWith([fixture.folder], out, false), 0, out.lines.join("\n"));
    const text = out.lines.join("\n");
    assert.ok(
      out.lines.includes("▲ Dependencies are not installed, so the index here was read without their types. Run `npm install` in web, then carrick refresh."),
      text,
    );
    assert.doesNotMatch(text, /npm install` in api/);
  } finally {
    fixture.restore();
  }
});

test("on that workspace a release mismatch and missing dependencies name no scan", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({ repos: { api: "indexed", web: "indexed" }, hostedState: "version_mismatch", notInstalled: ["api"] });
  try {
    const out = silentTerminal();
    assert.equal(await initWith([fixture.folder], out, false), 0, out.lines.join("\n"));
    const text = out.lines.join("\n");
    assert.ok(out.lines.includes(BUILT_BY_ANOTHER_RELEASE), text);
    assert.ok(out.lines.includes(`▲ ${READ_WITHOUT_TYPES}`), text);
    assert.doesNotMatch(text, /carrick index|before the next scan|scaffold/);
    // Nothing was read in, so the run does not say the index is here.
    assert.equal(out.lines.includes(`◇ ${READY}`), false);
  } finally {
    fixture.restore();
  }
  assert.equal(hostedReport({ kind: "version_mismatch", services: 2, reread: true }, 3).text, BUILT_BY_ANOTHER_RELEASE);
  assert.equal(installSentence(["`npm install` in api"]), READ_WITHOUT_TYPES);
  assert.equal(installSentence([]), null);
});

test("on that workspace --project is taken only where it changes nothing", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({ repos: { api: "indexed" } });
  try {
    const refused = silentTerminal();
    assert.equal(await initWith(["--project", "billing", fixture.repo("api")], refused, true), 1, refused.lines.join("\n"));
    assert.ok(refused.lines.includes(`■ ${SYSTEM_ON_DASHBOARD}`), refused.lines.join("\n"));
    assert.deepEqual(fixture.asked, ["resolve-repos acme/api"]);
    assert.deepEqual(fs.readdirSync(fixture.repo("api")).sort(), [".git", "package.json"]);
    // The project the repo is already in is not a change.
    const taken = silentTerminal();
    assert.equal(await initWith(["--project", "shop", fixture.repo("api")], taken, true), 0, taken.lines.join("\n"));
    assert.ok(taken.lines.includes("◇ Set up in project Shop (shop): api"), taken.lines.join("\n"));
  } finally {
    fixture.restore();
  }
});

test("on that workspace repos in two projects get a line each, and skills that take the repo", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({ repos: { api: "indexed", web: "indexed" }, projects: { web: "billing" } });
  try {
    const out = silentTerminal();
    assert.equal(await initWith([fixture.folder], out, true), 0, out.lines.join("\n"));
    assert.ok(out.lines.includes("◇ Set up in project Shop (shop): api"), out.lines.join("\n"));
    assert.ok(out.lines.includes("◇ Set up in project Billing (billing): web"), out.lines.join("\n"));
    const skill = fs.readFileSync(path.join(fixture.folder, taskSkillPaths()[0]!), "utf8");
    assert.match(skill, /repo: "<owner\/repo>"/);
    assert.doesNotMatch(skill, /project: "/);
    // --project cannot be true of both, so it is refused.
    const refused = silentTerminal();
    assert.equal(await initWith(["--project", "shop", fixture.folder], refused, true), 1);
    assert.ok(refused.lines.includes(`■ ${SYSTEM_ON_DASHBOARD}`), refused.lines.join("\n"));
  } finally {
    fixture.restore();
  }
});

test("on that workspace an editor found on this machine gets its entry unasked, and one that is not gets none", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({ repos: { api: "indexed" }, editors: { cursor: true, windsurf: false } });
  try {
    const out = silentTerminal();
    assert.equal(await initWith([fixture.repo("api")], out, true), 0, out.lines.join("\n"));
    assert.deepEqual(out.questions, []);
    const home = path.join(fixture.root, "home");
    const cursor = path.join(home, ".cursor", "mcp.json");
    assert.match(fs.readFileSync(cursor, "utf8"), /api\.carrick\.tools\/mcp/);
    assert.equal(fs.existsSync(path.join(home, ".codeium", "windsurf", "mcp_config.json")), false);
    // And the file is named in the list before it is written.
    assert.match(out.lines.join("\n").replace(/\n +/g, " "), /add Carrick's MCP server to Claude Code and ~\/\.cursor\/mcp\.json/);
  } finally {
    fixture.restore();
  }
});

// carrick#3361: there is one lane. The laptop lane saved the derive output to
// `.carrick/proposal.json` for a local agent to write carrick.json from, and
// the config now comes only from Carrick, so nothing of that lane is left to
// reach: not by a flag, not by a workspace read that does not say "cloud", not
// by a read that fails, and not by a folder whose repos name no GitHub repo.
test("init has one lane: --local and --allow-move are unknown, and the help names no proposal or config to write", () => {
  assert.equal(parseArgs(["--local"]), "unknown option for `carrick init`: --local");
  assert.equal(parseArgs(["--allow-move"]), "unknown option for `carrick init`: --allow-move");
  // Still accepted, for the scripts that pass it; init asks nothing after
  // sign-in, so it answers nothing.
  assert.equal(typeof parseArgs(["--yes"]), "object");
  const help = parseArgs(["--help"]);
  assert.equal(typeof help, "string");
  assert.doesNotMatch(String(help), /proposal|carrick\.json|--local|--allow-move|--yes|agent turns/);
});

test("init on a fresh workspace writes no proposal and prints no instruction to write a config", posixNativeFixture, async () => {
  for (const interactive of [true, false]) {
    const fixture = cloudWorkspace({ repos: { api: "connected" } });
    const out = silentTerminal();
    try {
      assert.equal(await initWith([fixture.repo("api")], out, interactive), 0, out.lines.join("\n"));
      assert.deepEqual(out.questions, []);
      const text = out.lines.join("\n");
      assert.doesNotMatch(text, /proposal|carrick\.json|carrick index|scaffold|paste this|agent (writes|turns)/i);
      const repo = fixture.repo("api");
      assert.equal(fs.existsSync(path.join(repo, ".carrick", "proposal.json")), false);
      assert.equal(fs.existsSync(path.join(repo, "carrick.json")), false);
      // The local tools are still installed.
      assert.equal(fs.existsSync(path.join(repo, CODEX_HOOKS_FILE)), true);
      for (const file of taskSkillPaths()) assert.equal(fs.existsSync(path.join(repo, file)), true, file);
    } finally {
      fixture.restore();
    }
  }
});

test("a workspace read that does not say cloud is the same one run, line for line and request for request", posixNativeFixture, async () => {
  const run = async (builder: string | null): Promise<{ text: string; asked: string[]; questions: string[]; commands: string[] }> => {
    const fixture = cloudWorkspace({ repos: { api: "indexed" }, builder });
    const out = silentTerminal();
    try {
      assert.equal(await initWith([fixture.repo("api")], out, true), 0, out.lines.join("\n"));
      return { text: printed(out, fixture.root), asked: [...fixture.asked], questions: out.questions, commands: fixture.commands() };
    } finally {
      fixture.restore();
    }
  };
  const cloud = await run("cloud");
  assert.deepEqual(cloud.questions, []);
  // A server older than the field, the value one excluded workspace is
  // answered with, and a word this build has never seen.
  for (const builder of [null, "laptop", "someday"]) assert.deepEqual(await run(builder), cloud, String(builder));
});

test("a folder whose repos name no GitHub repository stops before any request, and writes nothing", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({ repos: { api: "indexed" } });
  try {
    execFileSync("git", ["-C", fixture.repo("api"), "remote", "remove", "origin"]);
    const out = silentTerminal();
    assert.equal(await initWith(["--yes", fixture.repo("api")], out, false), 1, out.lines.join("\n"));
    assert.deepEqual(fixture.asked, []);
    assert.match(out.lines.join("\n"), /Carrick has nothing to connect it to/);
    assert.deepEqual(fs.readdirSync(fixture.repo("api")).sort(), [".git", "package.json"]);
  } finally {
    fixture.restore();
  }
});

test("a workspace read that fails stops the run and writes nothing", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({ repos: { api: "indexed" } });
  try {
    globalThis.fetch = (async () => {
      throw new Error("offline");
    }) as unknown as typeof fetch;
    const out = silentTerminal();
    assert.equal(await initWith([fixture.repo("api")], out, true), 1, out.lines.join("\n"));
    assert.deepEqual(out.questions, []);
    assert.deepEqual(fs.readdirSync(fixture.repo("api")).sort(), [".git", "package.json"]);
  } finally {
    fixture.restore();
  }
});

// carrick#1512 without the proposal file it used to read: a run inside one
// repo of a folder init has set up sets up the folder again.
test("inside a repo of a folder init set up, the run sets up the folder again, and no proposal is written anywhere", posixNativeFixture, async () => {
  const fixture = cloudWorkspace({ repos: { api: "indexed", web: "indexed" } });
  try {
    const first = silentTerminal();
    assert.equal(await initWith([fixture.folder], first, true), 0, first.lines.join("\n"));
    const again = silentTerminal();
    assert.equal(await initWith([fixture.repo("api")], again, true), 0, again.lines.join("\n"));
    assert.ok(
      again.lines.includes(` api is set up with the repos in ${fixture.folder}, so this run sets up ${fixture.folder}.`),
      again.lines.join("\n"),
    );
    assert.ok(again.lines.includes("◇ Set up in project Shop (shop): api, web"), again.lines.join("\n"));
    for (const dir of [fixture.folder, fixture.repo("api"), fixture.repo("web")]) {
      assert.equal(fs.existsSync(path.join(dir, ".carrick", "proposal.json")), false, dir);
    }
  } finally {
    fixture.restore();
  }
});

test("a hosted index another release built names no scan to run, on every workspace", () => {
  assert.equal(hostedReport({ kind: "version_mismatch", services: 2, reread: true }).text, BUILT_BY_ANOTHER_RELEASE);
  assert.equal(hostedReport({ kind: "version_mismatch", services: 2, reread: true }, 3).text, BUILT_BY_ANOTHER_RELEASE);
});

// A team can point `.claude/skills` at its own product's `skills/` folder. A
// skill written through that link lands in the product, so init writes none
// there, says so in one line, and carries on with the rest.
for (const harness of [".claude", ".agents"]) {
  test(`init writes no task skill through a linked ${harness}/skills and names the link`, posixNativeFixture, () => {
    const fixture = executableInitFixture("payments", "absent", "monorepo");
    try {
      const product = path.join(fixture.repo, "skills");
      fs.mkdirSync(path.join(product, "own"), { recursive: true });
      fs.writeFileSync(path.join(product, "own", "SKILL.md"), "mine\n");
      fs.mkdirSync(path.join(fixture.repo, harness), { recursive: true });
      fs.symlinkSync(path.join("..", "skills"), path.join(fixture.repo, harness, "skills"));
      const result = spawnSync(
        process.execPath,
        [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--yes", fixture.repo],
        { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
      );
      assert.equal(result.status, 0, result.stderr);
      // The product's folder holds only what it held.
      assert.deepEqual(pathsUnder(product), [path.join("own", "SKILL.md")]);
      assert.match(
        result.stdout,
        new RegExp(`${harness.replace(".", "\\.")}/skills is a link to \\.\\./skills, outside its own folder, so the Carrick task skills were not installed there`),
      );
      // The other harness is an ordinary folder and gets all four, and the
      // rest of init carried on.
      const other = harness === ".claude" ? ".agents" : ".claude";
      for (const skill of taskSkillPaths().filter((p) => p.startsWith(other))) {
        assert.ok(fs.existsSync(path.join(fixture.repo, skill)), skill);
      }
      assert.ok(fs.existsSync(path.join(fixture.repo, "AGENTS.md")));
    } finally {
      fixture.cleanup();
    }
  });
}

test("a task skill root that is an ordinary folder, or a link that stays inside its harness folder, still gets all four", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-skill-link-"));
  try {
    fs.mkdirSync(path.join(dir, ".agents", "real"), { recursive: true });
    fs.symlinkSync("real", path.join(dir, ".agents", "skills"));
    const outcomes = writeTaskSkills(dir, { slug: null });
    assert.equal(outcomes.length, 8);
    assert.ok(outcomes.every((row) => row.state === "absent" && row.wrote), JSON.stringify(outcomes));
    assert.deepEqual(taskSkillWarnings(outcomes), []);
    assert.ok(fs.existsSync(path.join(dir, ".agents", "real", "carrick-impact", "SKILL.md")));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("removing task skills deletes nothing through a link out of the harness folder, and doctor calls them skipped", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-skill-link-"));
  try {
    writeTaskSkills(dir, { slug: null });
    // Move .claude/skills into the product and link to it, as the customer has.
    fs.renameSync(path.join(dir, ".claude", "skills"), path.join(dir, "skills"));
    fs.symlinkSync(path.join("..", "skills"), path.join(dir, ".claude", "skills"));
    const before = pathsUnder(path.join(dir, "skills"));
    const lines = checkTaskSkills(inspectTaskSkills(dir));
    assert.ok(lines.every((line) => !/missing/.test(JSON.stringify(line))), JSON.stringify(lines));
    assert.match(JSON.stringify(lines), /not installed/);
    const { deleted } = removeTaskSkills(dir);
    assert.deepEqual(deleted.filter((p) => p.startsWith(".claude")), []);
    assert.deepEqual(pathsUnder(path.join(dir, "skills")), before);
    // The ordinary root was still taken back.
    assert.ok(deleted.some((p) => p.startsWith(".agents")));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
