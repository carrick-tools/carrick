// What `carrick init` decides before it writes anything.
//
// The two that matter most are the two that can destroy something: the repo
// list a user edited by hand, and a settings file that already holds someone
// else's hooks. Both are merges, and both are tested for what they leave alone.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  deriveWorkspace,
  repoIdentity,
  selectedProposal,
  siblingRepos,
  writeProposal,
  PROPOSAL_FILE,
  type WorkspaceProposal,
} from "../src/init/repos.ts";
import {
  carrickHooks,
  hookCommand,
  mergeCarrickHooks,
  ownEntryPoint,
} from "../src/init/settings.ts";
import { PROJECT_RULE, planProject, projectStep, reposPhrase, type Project, type ProjectPrompts } from "../src/init/projects.ts";
import {
  absentRepos,
  installCommands,
  installSentence,
  summaryLine,
  nextLines,
  nextBlock,
  wrapped,
  connectItem,
  writesLine,
  parseArgs,
  init,
  initWith,
  namedButExcluded,
  CANCELLED,
  NOTHING_WRITTEN,
  scaffoldSentence,
  reposToScaffold,
  preselectedRepos,
  chooseEditors,
  type InitOptions,
} from "../src/init/run.ts";
import { taskSkillPaths } from "../src/init/task-skills.ts";
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
import type { NativeRun } from "../src/init/hosted.ts";
import type { RunningScan, StatusResult, StatusService } from "../src/contract.ts";
import {
  chosenNumbers,
  DOCS,
  interactiveOutput,
  plainOutput,
  PromptCancelled,
  type Choice,
  type InitOutput,
} from "../src/init/output.ts";
import { adminWait } from "../src/init/connect.ts";
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

// The seam document carrick-cloud#799 pins: the scaffold tool's agent reads
// it, so what lands on disk has to be the scanner's own bytes, not a
// re-serialisation of what this client could parse.
/** The closing instruction for one repo, as the fixtures name it. */
function scaffoldFor(name: string): string {
  return scaffoldSentence([{ path: `/repos/${name}`, name, remote: null, problem: null }]);
}

test("the proposal is written as the scanner printed it, into an ignored directory", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-init-"));
  try {
    const plan: WorkspaceProposal = { schema: "carrick.derive/0", workspace: dir, repos_detected_by: "single repository", repos_added: [], repos_excluded: [], missing: [], parent_proposal: null, repos: [{ path: dir, reason: "single repository", services: [{ serviceName: "api" }], config: { services: [{ name: "api", include: ["shared"] }] }, warnings: [], not_installed: [] }] };
    // A field this client's schema does not know: a later carrick.derive/0 may
    // add one, and the agent must still receive it.
    const document = JSON.stringify({ ...plan, unknown_to_this_client: ["keep me"] });
    assert.equal(writeProposal(dir, { plan, document }), PROPOSAL_FILE);
    const written = fs.readFileSync(path.join(dir, PROPOSAL_FILE), "utf8");
    assert.equal(written, `${document}\n`);
    assert.deepEqual(JSON.parse(written).unknown_to_this_client, ["keep me"]);

    // Nothing under .carrick is ever committed, whichever command created it.
    assert.equal(fs.readFileSync(path.join(dir, ".carrick", ".gitignore"), "utf8").trimEnd().split("\n").at(-1), "*");

    // Derived, so re-running replaces it rather than preserving a stale seed.
    const second = { plan, document: JSON.stringify(plan) };
    writeProposal(dir, second);
    assert.equal(fs.readFileSync(path.join(dir, PROPOSAL_FILE), "utf8"), `${second.document}\n`);

    // And it is the only thing written: no carrick.json, at any depth.
    assert.deepEqual(fs.readdirSync(dir).sort(), [".carrick"]);

    // A selection that keeps every repo is the scanner's own document, not a
    // re-serialisation of it: the filtering that scopes a folder's proposal to
    // the repos this install covers hands back what it was given
    // (carrick#1338).
    const derived = { plan, document };
    assert.equal(selectedProposal(derived, [dir]), derived);
    // And a selection that drops one keeps every field this client's schema
    // does not know, because it filters the scanner's JSON rather than the
    // shape that parsed out of it.
    const dropped = selectedProposal(derived, []);
    assert.deepEqual(dropped.plan.repos, []);
    assert.deepEqual(dropped.plan.repos_excluded, [path.basename(dir)]);
    assert.deepEqual(JSON.parse(dropped.document).unknown_to_this_client, ["keep me"]);
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
        SessionStart: [
          { hooks: [{ type: "command", command: '"$CLAUDE_PROJECT_DIR/.claude/session-start.sh"' }] },
        ],
        PreToolUse: [
          { matcher: "Grep", hooks: [{ type: "command", command: '"$CLAUDE_PROJECT_DIR/.claude/search-gate.sh"' }] },
        ],
      },
    },
    null,
    2,
  );
  const merged = mergeCarrickHooks(existing);
  const written = JSON.parse(merged.body);
  assert.deepEqual(written.permissions, { allow: ["Bash(ls:*)"] });
  assert.equal(written.hooks.PreToolUse.length, 1);
  // The hosted index's session-start script survives, with ours after it.
  assert.equal(written.hooks.SessionStart.length, 2);
  assert.match(written.hooks.SessionStart[0].hooks[0].command, /session-start\.sh/);
  assert.equal(written.hooks.SessionStart[1].hooks[0].command, "carrick hook session-start");
  assert.equal(written.hooks.PostToolUse[0].hooks[0].command, "carrick hook post-edit");
  // The end-of-task reuse nudge (carrick#1330). Installed beside the other two
  // rather than by anything the user has to add.
  assert.equal(written.hooks.Stop[0].hooks[0].command, "carrick hook stop");
  assert.equal(written.hooks.Stop[0].matcher, undefined, "a Stop hook has no matcher");
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
  assert.deepEqual(commands, ["eslint --fix", "carrick hook post-edit"]);
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
  assert.equal(rewritten.hooks.PostToolUse.length, 1);
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
  assert.deepEqual(commands, ["carrickctl hook post-edit", "carrick hook post-edit"]);
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
    assumeYes: true,
    allowMove: false,
    project: "payments",
    repos: [],
    editors: [],
    installGlobal: false,
  });
  // A global install is a change to the machine, not to this workspace, so
  // `--yes` is not it either (carrick#1372).
  assert.equal((parseArgs(["--yes"]) as InitOptions).installGlobal, false);
  assert.equal((parseArgs(["--install-global"]) as InitOptions).installGlobal, true);
  // The editors an entry may be written for, named the way init prints them.
  // Nothing here is a default: a run with no terminal writes no editor file
  // unless this flag names one (carrick#1365).
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
  // A move is never granted by --yes, so it has a flag of its own.
  assert.equal((parseArgs(["--yes"]) as InitOptions).allowMove, false);
  assert.equal((parseArgs(["--allow-move"]) as InitOptions).allowMove, true);
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
  // The help said "Require these repos in this Carrick project" and never that
  // it moves them out of the one they are in (carrick#1338).
  assert.match(help, /is MOVED out of it/);
  assert.match(help, /--allow-move/);
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
    assert.throws(() => deriveWorkspace(dir), /unsupported workspace proposal/);
  } finally {
    restoreEnv();
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// carrick#993 row 8. An unverified `--project` used to exit 1 before the hooks
// and the proposal were written, so the documented command needed two runs:
// one to be told to open a browser, another to get the setup it came for. It
// now finishes the local half and says which browser steps are left.
test("the executable CLI finishes setup when the named project is not verified", posixNativeFixture, () => {
  const fixture = executableInitFixture("default-project");
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", "--allow-move", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /^ {2}Move repo from default-project into payments$/m);
    assert.match(result.stdout, /Create project "payments" if needed/);
    // This API serves no assignment action, so the browser keeps that step.
    assert.match(result.stdout, /Assign the requested repos/);
    // The project actions are not deployed, so nothing was listed and the
    // browser still owns creating it.
    assert.doesNotMatch(result.stdout, /Projects in this workspace/);
    assert.doesNotMatch(result.stdout, /Created project/);
    assert.doesNotMatch(result.stdout, /Verified/);
    assert.ok(
      result.stdout.includes(
        "▲ Finish the browser steps above to put these repos in payments, then run carrick init --project payments again to verify.",
      ),
      result.stdout,
    );
    // The setup it came for, written: everything but a config it did not derive.
    assert.equal(fs.existsSync(path.join(fixture.repo, "carrick.json")), false);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".claude")), true);
    assert.equal(fs.existsSync(path.join(fixture.repo, PROPOSAL_FILE)), true);
  } finally {
    fixture.cleanup();
  }
});

// carrick#1338. `carrick init -w <folder> --project <slug>` moved two repos
// out of their projects as its FIRST act, printed the proposal afterwards, and
// then refused to go on for want of a terminal. The moves were already done;
// nothing was written locally. The order is the fix, and the order is what
// these pin: every question is asked before the first request that changes
// anything, so a run that stops leaves both sides as it found them.
test("no assignment is issued before the proposal is answered", posixNativeFixture, () => {
  const fixture = executableInitFixture("default-project", "deployed");
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 1, result.stdout);
    // Reads, and only reads. `assign-repos` and `create-project` are the two
    // actions that change the server, and neither was asked for.
    assert.deepEqual(fixture.requests(), ["resolve-repos acme/api", "list-projects"]);
    // The move the run would have made, named with the project it comes out
    // of — display name and slug, which is what the dashboard shows it under —
    // in the list the one question is about, beside the create (carrick#1512).
    assert.match(
      result.stdout,
      /^Next:\n {2}Create project payments with repo\n {2}Move repo from Acme Default \(default-project\) into payments$/m,
    );
    assert.match(result.stderr, /use --yes to accept this proposal without a terminal, and --allow-move/);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".carrick")), false);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".claude")), false);
  } finally {
    fixture.cleanup();
  }
});

test("--yes accepts the proposal and still does not grant a move", posixNativeFixture, () => {
  const fixture = executableInitFixture("default-project", "deployed");
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 1, result.stdout);
    assert.deepEqual(fixture.requests(), ["resolve-repos acme/api", "list-projects"]);
    // Named as the list names it: the folder, not owner/repo (carrick#1512).
    assert.match(result.stderr, /repo is in project Acme Default \(default-project\)/);
    assert.match(result.stderr, /needs --allow-move/);
    // A refused move writes nothing either: the hooks and the proposal are
    // scoped to a project this run could not settle.
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

// carrick#1338 item 4: the only answer this command took was one yes covering
// every repo in the folder, so the only safe answer for a folder holding a repo
// that must not be scanned was No — which also skipped the hooks and the skills
// for the repos that did belong.
test("a repo left out of the selection gets no proposal entry and no assignment", posixNativeFixture, () => {
  const fixture = folderInitFixture();
  try {
    const result = spawnSync(
      process.execPath,
      [
        path.join(packageRoot, "bin", "carrick.mjs"), "init",
        "--repo", "acme/api", "--project", "payments", "--yes", "--allow-move", fixture.folder,
      ],
      { cwd: fixture.folder, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /1 of 2 repos covered/);

    // The proposal an agent turns into carrick.json holds the covered repo and
    // nothing else, and says which repo was left out.
    const proposal = JSON.parse(fs.readFileSync(path.join(fixture.folder, PROPOSAL_FILE), "utf8"));
    assert.deepEqual(proposal.repos.map((repo: { path: string }) => repo.path), [fixture.api]);
    assert.deepEqual(proposal.repos_excluded, ["web"]);

    // Carrick was never told about it: not in the workspace read, not in the
    // assignment, not in the project.
    for (const request of fixture.requests()) assert.doesNotMatch(request, /acme\/web/);
    assert.ok(fixture.requests().includes("assign-repos acme/api"), fixture.requests().join("\n"));

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

test("a folder of repos with nothing naming a selection writes nothing at all", posixNativeFixture, () => {
  const fixture = folderInitFixture();
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", fixture.folder],
      { cwd: fixture.folder, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 1, result.stdout);
    assert.match(result.stderr, /holds 2 repos and there is no terminal to choose in/);
    assert.match(result.stderr, /--repo owner\/repo/);
    // Before the login, before the workspace read: nothing was asked of
    // Carrick and nothing was written on this machine.
    assert.deepEqual(fixture.requests(), []);
    for (const entry of [".carrick", ".claude", ".agents"]) {
      assert.equal(fs.existsSync(path.join(fixture.folder, entry)), false, entry);
    }
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
      assert.match(result.stdout, /^◇ payments: repo$/m);
      assert.doesNotMatch(result.stdout, /Verified 1 repo in project|Signed in as/);
      assert.doesNotMatch(result.stdout, /Create project "payments" if needed/);
      // A repo already in the project is not a project to look up or create.
      assert.doesNotMatch(result.stdout, /Projects in this workspace/);
      // Setup ends where the dashboard's checklist ends: one sentence naming
      // the scaffold tool, which carries the instructions (cloud#832).
      assert.ok(result.stdout.includes(scaffoldFor("acme/api")), result.stdout);
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

// The terminal half of the project step, against an API that has the actions:
// the list is printed, the project is created from here, the repos are moved
// into it from here, and the browser is left with the App grant alone
// (carrick#999).
test("the executable CLI creates the named project and puts the repos in it", posixNativeFixture, () => {
  const fixture = executableInitFixture("default-project", "deployed");
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", "--allow-move", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Projects in this workspace:/);
    // Display name beside slug, wherever a project is printed: the dashboard's
    // picker shows the name and every CLI line showed the slug (carrick#1338).
    assert.match(result.stdout, /^ {2}Default \(default\) {2}1 repo$/m);
    // Not said on a line of its own: the list named it and the line after the
    // connection names it with its repos (carrick#1512).
    assert.doesNotMatch(result.stdout, /Created project/);
    assert.doesNotMatch(result.stdout, /Create project "payments" if needed/);
    // The move is made. The repo was in a project somebody chose, so it is
    // said, once (carrick#1489 review).
    assert.deepEqual(fixture.requests().filter((request) => request !== "list-projects"), [
      "resolve-repos acme/api",
      "create-project",
      // Read again after the create, before anything is decided from it.
      "resolve-repos acme/api",
      "assign-repos acme/api",
      "resolve-repos acme/api",
    ]);
    assert.equal(
      result.stdout.split("\n").filter((line) => line === "Moved acme/api from Acme Default (default-project) into payments.").length,
      1,
      result.stdout,
    );
    // The move is named in the list, with the project it comes out of, and
    // it is named BEFORE the request that performs it.
    const proposed = result.stdout.indexOf("  Move repo from Acme Default (default-project) into payments");
    assert.ok(proposed >= 0, result.stdout);
    assert.ok(proposed < result.stdout.indexOf("Moved acme/api"), result.stdout);
    // Claimed only because resolve-repos read it back afterwards.
    assert.match(result.stdout, /^◇ payments: repo$/m);
    // And no browser step is asked for, because none is left.
    assert.doesNotMatch(result.stdout, /Assign the requested repos/);
    assert.doesNotMatch(result.stdout, /Setup continues/);
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
    // And nothing tells the reader to run a free pass by hand: the read above
    // is this command's, and `carrick index` stays the only scan in the flow
    // (carrick#1008, cloud#832).
    assert.doesNotMatch(result.stdout, /carrick refresh/);
    // Both branches end on the same sentence: whether a scan runs at all is
    // the scaffold tool's to state, from the repo it is asked about
    // (cloud#832), so the terminal carries no second copy of it.
    assert.ok(result.stdout.trimEnd().endsWith(scaffoldFor("acme/api")), result.stdout);
    assert.ok(result.stdout.includes(scaffoldFor("acme/api")), result.stdout);
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
    assert.ok(
      result.stdout.includes(
        "▲ Hosted index is older than this CLI: run `carrick index --detach` once from main",
      ),
      result.stdout,
    );
    assert.doesNotMatch(result.stdout, /npm i -g carrick@/);
    // And no claim that the hosted rows arrived, because they did not.
    assert.doesNotMatch(result.stdout, /downloaded into \.carrick/);
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
    // the closing block is the one every other branch ends on.
    assert.ok(result.stdout.trimEnd().endsWith(scaffoldFor("acme/api")), result.stdout);
    assert.equal(fs.existsSync(path.join(fixture.repo, PROPOSAL_FILE)), true);
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
// the task skills the two harnesses read: carrick.json is the agent's to write,
// after someone has read it, and before the one paid scan
// (carrick-cloud#799). A change to this set is a deliberate diff in this list.
test("a first init writes the proposal, its ignore file, the hook settings and the task skills, and nothing else", posixNativeFixture, () => {
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
    assert.match(result.stdout, /^◇ payments: repo$/m);

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
        PROPOSAL_FILE,
        path.join(".claude", onPath ? "settings.json" : "settings.local.json"),
        // The same two-part nudge, for the other host (carrick#1335).
        CODEX_HOOKS_FILE,
        ...taskSkillPaths(),
      ].sort(),
    );

    // The proposal is the whole derivation, including the config it would once
    // have written into the tree.
    // And the set is ignored where it has to be: all git can see in the tree
    // after a first run is the three agent directories, which hold the settings,
    // the Codex hooks and the skills and are meant to be committed.
    assert.equal(
      execFileSync("git", ["-C", fixture.repo, "status", "--porcelain"], { encoding: "utf8" }),
      "?? .agents/\n?? .claude/\n?? .codex/\n",
    );

    const proposal = JSON.parse(fs.readFileSync(path.join(fixture.repo, PROPOSAL_FILE), "utf8"));
    assert.equal(proposal.schema, "carrick.derive/0");
    assert.equal(proposal.repos[0].services.length, 16);
    assert.equal(proposal.repos[0].config.services.length, 16);
    assert.equal(proposal.workspace, fixture.repo);

    // And the run ends in three parts (carrick#1489): what is set up, in one
    // line; what is left to do, a line each; and the one sentence for the
    // agent. This workspace read reports no services, so there is no index
    // step between them. No "Restart Claude Code" line: the block says a NEW
    // agent session, which is the one that loads the server (carrick#1512).
    assert.equal(
      result.stdout.trimEnd().split("\n").slice(-5).join("\n"),
      [
        "◇ payments: repo",
        "▲ Codex asks you to trust the Carrick hooks the next time it starts; until you do, they do not run.",
        "",
        "Next: paste this into a new agent session",
        `  ${scaffoldFor("acme/api")}`,
      ].join("\n"),
      result.stdout,
    );
    assert.doesNotMatch(result.stdout, /Restart Claude Code/);
    // The quickstart carries what left the terminal, and none of it is printed.
    assert.doesNotMatch(result.stdout, /--plugin-dir|--install-extension|carrick templates workflow|docs\.carrick\.tools\/carrick-json/);
  } finally {
    fixture.cleanup();
  }
});

function recordingPrompts(
  overrides: Partial<ProjectPrompts> = {},
): ProjectPrompts & { lines: string[]; asked: string[] } {
  const lines: string[] = [];
  const asked: string[] = [];
  return {
    lines,
    asked,
    repos: ["api", "web"],
    say: (line: string) => void lines.push(line),
    ask: async (question: string) => {
      asked.push(question);
      return "";
    },
    confirm: async (question: string) => {
      asked.push(question);
      return true;
    },
    pick: async (question: string) => {
      asked.push(question);
      throw new Error("no pick list was expected");
    },
    interactive: false,
    assumeYes: false,
    ...overrides,
  };
}

/** A pick list answered with the row whose label is `label`, in order, once each. */
function picking(...labels: string[]): ProjectPrompts["pick"] {
  return async (_question, options) => {
    const label = labels.shift();
    const row = options.find((option) => option.label === label);
    if (row === undefined) throw new Error(`no row "${label}" in ${options.map((option) => option.label).join(", ")}`);
    return row.value;
  };
}

const LISTED: Project[] = [
  { slug: "default", name: "Default", archived: false, repo_count: 4 },
  { slug: "payments", name: "Payments", archived: false, repo_count: 1 },
];

// carrick#987 item 3: a plain `carrick init` used to skip the project step
// entirely, so a first run never saw the workspace's projects.
test("a plain init takes the project the repos are already in", async () => {
  const prompts = recordingPrompts();
  assert.deepEqual(await projectStep(["payments", "payments"], LISTED, prompts), {
    slug: "payments",
    exists: true,
    create: false,
  });
  // The caller prints the assignment it then verifies, so this step adds no
  // second sentence about it.
  assert.deepEqual(prompts.lines, []);
  assert.deepEqual(prompts.asked, []);
});

test("a plain init asks nothing without a terminal, and settles nothing it would have to guess", async () => {
  for (const current of [["payments", "billing"], [null], []]) {
    const prompts = recordingPrompts();
    assert.deepEqual(await projectStep(current, LISTED, prompts), { slug: null, exists: false, create: false });
    assert.deepEqual(prompts.asked, []);
  }
});

// carrick#1489 part 1: the question asked for a slug and hid what Enter did.
// It is one pick list now, and a new project is asked for by its name.
test("a plain init creates a new project from the name the terminal types", async () => {
  const pick = picking("New project");
  const prompts = recordingPrompts({
    interactive: true,
    pick: async (question, options) => {
      prompts.asked.push(question);
      return pick(question, options);
    },
    ask: async (question) => {
      prompts.asked.push(question);
      return "Acme Search";
    },
    confirm: async (question) => {
      prompts.asked.push(question);
      return true;
    },
  });
  // Named, and not confirmed here: the create is a line in the list "Go
  // ahead?" is asked about, which is where the reader sees which repos go
  // into it, and nothing is created before that answer (carrick#1338,
  // carrick#1512). The question itself names the repos.
  assert.deepEqual(await projectStep([null, "payments"], LISTED, prompts), {
    slug: "acme-search",
    exists: false,
    create: true,
    name: "Acme Search",
  });
  assert.deepEqual(prompts.asked, ["Which project should api and web be in?", "Project name"]);
  // What a project is, said above the question rather than in the docs: a
  // project is the boundary every cross-repo answer is computed inside, and
  // splitting one system across two of them is the mistake this prevents
  // (carrick#993 row 13).
  assert.deepEqual(prompts.lines, [PROJECT_RULE]);
  // And the word the first run never needed is not in any of it.
  assert.ok([...prompts.lines, ...prompts.asked].every((line) => !/slug/i.test(line)));
});

test("a plain init takes a listed project without creating anything", async () => {
  const prompts = recordingPrompts({
    interactive: true,
    pick: picking("Payments (payments)"),
    confirm: async () => {
      throw new Error("an existing project is not one to offer to create");
    },
  });
  assert.deepEqual(await projectStep([null], LISTED, prompts), {
    slug: "payments",
    exists: true,
    create: false,
  });
});

test("a name that clashes is refused and asked again; no name goes back to the list", async () => {
  const names = ["Payments", "Billing"];
  const clash = recordingPrompts({
    interactive: true,
    pick: picking("New project"),
    ask: async () => names.shift() ?? "",
  });
  assert.deepEqual(await projectStep([null], LISTED, clash), {
    slug: "billing",
    exists: false,
    create: true,
    name: "Billing",
  });
  assert.ok(clash.lines.includes("Project Payments (payments) already exists. Pick another name."), clash.lines.join("\n"));

  const unnamed = recordingPrompts({
    interactive: true,
    pick: picking("New project", "Default (default)"),
    ask: async () => "",
    confirm: async () => {
      throw new Error("a project is not confirmed at its own question (carrick#1512)");
    },
  });
  assert.deepEqual(await projectStep([null], LISTED, unnamed), { slug: "default", exists: true, create: false });
});

// carrick#1512: the keep question names the repos it is about, one or many.
test("the project questions name the repos they are about", async () => {
  const one = recordingPrompts({ interactive: true, repos: ["shop-app"], confirm: async (question) => {
    one.asked.push(question);
    return true;
  } });
  await projectStep(["payments"], LISTED, one);
  assert.deepEqual(one.asked, ["shop-app is in project Payments (payments). Keep it there?"]);
  const two = recordingPrompts({ interactive: true, repos: ["shop-app", "shop-api"], confirm: async (question) => {
    two.asked.push(question);
    return true;
  } });
  await projectStep(["payments", "payments"], LISTED, two);
  assert.deepEqual(two.asked, ["shop-app and shop-api are in project Payments (payments). Keep them there?"]);
  assert.equal(reposPhrase(["a", "b", "c", "d", "e"]), "a, b, c and 2 more");
});

// carrick#1512: a named project that is not there is created on the one
// "Go ahead?", not at a question of its own.
test("--project names a new project without asking to create it", async () => {
  const prompts = recordingPrompts({
    interactive: true,
    confirm: async () => {
      throw new Error("the create is a line in the list, not a question of its own");
    },
  });
  assert.deepEqual(await planProject("search", LISTED, prompts), { slug: "search", exists: false, create: true });
});

test("choosing the dashboard opens its projects page and says what that leaves undone", async () => {
  const none = { slug: null, exists: false, create: false };
  const opened: string[] = [];
  const url = "https://app.carrick.tools/w/acme/projects";
  const prompts = recordingPrompts({
    interactive: true,
    pick: picking("Choose on the dashboard"),
    dashboard: { url, open: (target) => void opened.push(target) },
  });
  assert.deepEqual(await projectStep([null], LISTED, prompts), none);
  assert.deepEqual(opened, [url]);
  assert.equal(prompts.lines.at(-1), `Add api and web to a project at ${url}. Until then they are in no project.`);

  // An API without the actions is every workspace until the cloud half ships,
  // and there is no list to pick from.
  const absent = recordingPrompts({ interactive: true });
  assert.deepEqual(await projectStep([null], null, absent), none);
  assert.deepEqual(absent.asked, []);
});

test("the executable CLI cannot verify a project without a GitHub repo identity", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments");
  try {
    execFileSync("git", ["-C", fixture.repo, "remote", "remove", "origin"]);
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 1);
    assert.match(result.stderr, /cannot verify .* because it has no GitHub origin/);
    assert.doesNotMatch(result.stdout, /Verified/);
  } finally {
    fixture.cleanup();
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
    assert.match(result.stdout, /^◇ payments: repo$/m);
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
    assert.equal(result.status, 0, result.stderr);
    // The path, the remote as written, and what ssh made of it.
    assert.ok(
      result.stdout.includes(
        `${fixture.repo} contributes no GitHub identity: its origin ${ALIAS_REMOTE} names the host "github.com-work"`,
      ),
      result.stdout,
    );
    assert.match(result.stdout, /carrick init --repo owner\/repo/);
    assert.match(result.stdout, /HostName github\.com line in your ssh config/);
    assert.match(result.stdout, /chooses no project and checks no connection/);
    // The local half still happened: this is a loud run, not a failed one.
    assert.equal(fs.existsSync(path.join(fixture.repo, PROPOSAL_FILE)), true);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".claude")), true);
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
    assert.match(result.stdout, /^◇ payments: repo$/m);
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
// The picker itself, in the rendering a terminal gets. It opens on the set it
// was given rather than on everything, every row says in text whether it is in
// or out, and the header counts them: clack's own multiselect draws the row
// under the cursor and an unselected row with the SAME glyph, which is what
// made "space to select" unreadable (carrick#1365).
test("the picker opens on its initial set, and marks every row in or out", async () => {
  const keys = new PassThrough();
  const rendered = new PassThrough();
  let seen = "";
  rendered.on("data", (chunk: Buffer) => void (seen += String(chunk)));
  const chosen = interactiveOutput(rendered, keys).choose(
    "Which repos does this install cover?",
    "a repo",
    [
      { value: "/repos/api", label: "acme/api", hint: "1 package" },
      { value: "/repos/data", label: "acme/data", hint: "3 packages, in Demo Repos" },
    ],
    { initial: ["/repos/api"], required: true },
  );
  // Down to the second row, space to add it, enter to confirm.
  setTimeout(() => keys.write("\u001b[B"), 30);
  setTimeout(() => keys.write(" "), 60);
  setTimeout(() => keys.write("\r"), 90);
  assert.deepEqual(await chosen, ["/repos/api", "/repos/data"]);
  assert.match(seen, /\[x\] acme\/api/);
  assert.match(seen, /\[ \] acme\/data/);
  assert.match(seen, /3 packages, in Demo Repos/);
  assert.match(seen, /Space toggles a repo, Enter confirms\. 1 of 2 selected\./);
  assert.match(seen, /2 of 2 selected/);
});

// The plain rendering has no cursor to move, so the picker is a numbered list
// and an answer naming numbers. Enter covers everything, which is the answer
// a folder of repos that all belong wants (carrick#1338).
test("the numbered picker takes numbers, all, none, or the marked set", () => {
  const some = { initial: [0], required: true };
  // Enter accepts what the rows are marked with, which is what the reader can
  // see — it used to mean "all of them", which is now a different answer.
  assert.deepEqual(chosenNumbers("", 3, some), [0]);
  assert.deepEqual(chosenNumbers("  ", 3, some), [0]);
  assert.deepEqual(chosenNumbers("ALL", 3, some), [0, 1, 2]);
  assert.deepEqual(chosenNumbers("1,3", 3, some), [0, 2]);
  assert.deepEqual(chosenNumbers("3 1 3", 3, some), [2, 0]);
  for (const answer of ["0", "4", "x", "1,x", "-1", "1.5"]) {
    assert.equal(chosenNumbers(answer, 3, some), null, answer);
  }
  // Nothing marked and an answer required: Enter is not an answer, and the
  // question is asked again.
  assert.equal(chosenNumbers("", 3, { initial: [], required: true }), null);
  assert.equal(chosenNumbers("none", 3, some), null);
  // Where an empty answer is one — the editors — both ways of saying it work.
  assert.deepEqual(chosenNumbers("", 3, { initial: [], required: false }), []);
  assert.deepEqual(chosenNumbers("NONE", 3, { initial: [1], required: false }), []);
});

test("a cancelled question is not an answer, in either rendering", async () => {
  const closing = new PassThrough();
  const plain = plainOutput(() => {}, { input: closing, output: new PassThrough() });
  const asked = plain.confirm("Write the proposal?");
  closing.end();
  await assert.rejects(asked, PromptCancelled);

  const ending = new PassThrough();
  const typed = plainOutput(() => {}, { input: ending, output: new PassThrough() });
  const question = typed.ask("Which project should these repos be in?");
  ending.end();
  await assert.rejects(question, PromptCancelled);

  // The library's own cancel, which is the one the report was made on: clack
  // answers Ctrl-C with a symbol rather than an error.
  const keys = new PassThrough();
  const rendered = new PassThrough();
  rendered.resume();
  const interactive = interactiveOutput(rendered, keys);
  // One at a time: two prompts reading one stream would let a single Ctrl-C
  // stand in for both, and each of the three has its own cancel to answer.
  for (const prompt of [
    () => interactive.confirm("Keep them there?"),
    () => interactive.ask("Project name"),
    () => interactive.pick("Which project should these repos be in?", [{ value: "new", label: "New project" }]),
    () =>
      interactive.choose(
        "Which repos should Carrick index?",
        "a repo",
        [{ value: "/repos/api", label: "acme/api" }],
        { initial: ["/repos/api"], required: true },
      ),
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
    choose: async (_question, _noun, options, config) =>
      options.filter((option) => config.initial.includes(option.value)).map((option) => option.value),
    pick: async () => {
      throw new Error("no pick list was expected");
    },
    accent: (text) => text,
    quiet: false,
    ...overrides,
  };
}

/**
 * One repo, in project `default-project`, with the server answered in this
 * process: the run-level questions are what these tests are about, so the
 * output is a fake and the terminal is stated rather than sniffed.
 */
function inProcessRepo(name: string): {
  repo: string;
  asked: string[];
  restore: () => void;
} {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), `carrick-init-${name}-`)));
  const repo = path.join(root, "repo");
  fs.mkdirSync(repo);
  execFileSync("git", ["init", "-q", repo]);
  execFileSync("git", ["-C", repo, "remote", "add", "origin", "git@github.com:acme/api.git"]);
  const native = path.join(root, "native.mjs");
  fs.writeFileSync(native, `#!/usr/bin/env node
const argv = process.argv.slice(2);
if (argv[0] !== "derive") process.exit(2);
const workspace = argv[argv.indexOf("--workspace") + 1];
process.stdout.write(JSON.stringify({
  schema: "carrick.derive/0", workspace, repos_detected_by: "single repository",
  repos_added: [], repos_excluded: [], missing: [], parent_proposal: null,
  repos: [{ path: workspace, reason: "single repository", services: [{ serviceName: "api" }], config: null, warnings: [], not_installed: [] }],
}));
`);
  fs.chmodSync(native, 0o755);
  const restoreEnv = withEnv({
    CARRICK_BIN: native,
    CARRICK_TOKEN: "test-token",
    XDG_CONFIG_HOME: path.join(root, "config"),
    HOME: path.join(root, "home"),
  });
  const fetchBefore = globalThis.fetch;
  const asked: string[] = [];
  globalThis.fetch = (async (_input: string, init: { body: string }) => {
    const body = JSON.parse(String(init.body));
    asked.push(body.action);
    if (body.action === "resolve-repos") {
      return Response.json({
        schema: "carrick.resolve-repos/0",
        workspace: { slug: "acme", billing_tier: "free", installed: true },
        allowance_sentence: null,
        repos: [{ full_name: "acme/api", connected: true, project_id: "p1", project_slug: "default-project", services: [] }],
        project_repos: [{ project_slug: "default-project", repos: ["acme/api"] }],
      });
    }
    return Response.json({
      schema: "carrick.list-projects/0",
      projects: [
        { slug: "default-project", name: "Acme Default", archived: false, repo_count: 1 },
        { slug: "payments", name: "Payments", archived: false, repo_count: 0 },
      ],
    });
  }) as unknown as typeof fetch;

  return {
    repo,
    asked,
    restore: () => {
      restoreEnv();
      globalThis.fetch = fetchBefore;
      fs.rmSync(root, { recursive: true, force: true });
    },
  };
}

test("a cancel at the project question ends the run with nothing written", async () => {
  const fixture = inProcessRepo("cancel");
  try {
    // The terminal is here, and the reader ends the question rather than
    // answering it.
    const out = recordingOutput({
      confirm: async () => {
        throw new PromptCancelled();
      },
    });
    assert.equal(await initWith([fixture.repo], out, true), 1);
    assert.ok(out.lines.includes(`■ ${CANCELLED}`), out.lines.join("\n"));
    // Nothing on this machine, and nothing the server was asked to change.
    assert.deepEqual(fs.readdirSync(fixture.repo), [".git"]);
    assert.deepEqual(fixture.asked, ["resolve-repos", "list-projects"]);
  } finally {
    fixture.restore();
  }
});

test("a declined proposal says so, and is the same nothing as a cancel", async () => {
  const fixture = inProcessRepo("declined");
  try {
    // "No" to keeping the project, the same project off the list, and "no" to
    // the write.
    const out = recordingOutput({
      confirm: async () => false,
      pick: async (_question, options) => options.find((option) => option.label === "Acme Default (default-project)")!.value,
    });
    assert.equal(await initWith([fixture.repo], out, true), 0);
    assert.ok(out.lines.includes(`■ ${NOTHING_WRITTEN}`), out.lines.join("\n"));
    assert.deepEqual(fs.readdirSync(fixture.repo), [".git"]);
    assert.deepEqual(fixture.asked, ["resolve-repos", "list-projects"]);
  } finally {
    fixture.restore();
  }
});

// `--yes` is an answer to the proposal, not to a move: with a terminal it
// skips the picker and the proposal question and still asks this one
// (carrick#1338).
test("--yes leaves one question, and it is the move", async () => {
  const fixture = inProcessRepo("yes");
  try {
    const questions: string[] = [];
    const out = recordingOutput({
      confirm: async (question: string) => {
        questions.push(question);
        return false;
      },
      choose: async () => {
        throw new Error("--yes takes the repo list as derived");
      },
    });
    assert.equal(await initWith(["--yes", "--project", "payments", fixture.repo], out, true), 0);
    // Spelled as the list above it spells the move: the folder name, and the
    // project as the dashboard labels it (carrick#1512).
    assert.deepEqual(questions, ["Move repo from Acme Default (default-project) into Payments (payments)?"]);
    const listed = wrapped("Move repo from Acme Default (default-project) into Payments (payments)").map((part) => `  ${part}`).join("\n");
    assert.ok(out.lines.some((line) => line.includes(listed)), out.lines.join("\n"));
    assert.deepEqual(fs.readdirSync(fixture.repo), [".git"]);
    assert.deepEqual(fixture.asked, ["resolve-repos", "list-projects"]);
  } finally {
    fixture.restore();
  }
});

/**
 * A first run on a fresh workspace after carrick-cloud#1359: no project at
 * all, the GitHub App installed on the repo while init sat at its questions,
 * and the first project created taking the repo the install staged.
 *
 * `claims` is what the create answers: the cloud since #1372 says how many
 * repos the new project took, the cloud before it says nothing, and there the
 * App grant put the repo in a default project that init then moves it out of.
 */
function firstRunRepo(claims: "adopts" | "before-1359" | "read-fails"): {
  repo: string;
  asked: string[];
  restore: () => void;
} {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-init-first-")));
  const repo = path.join(root, "acme-api");
  fs.mkdirSync(repo);
  execFileSync("git", ["init", "-q", repo]);
  execFileSync("git", ["-C", repo, "remote", "add", "origin", "git@github.com:acme/acme-api.git"]);
  fs.writeFileSync(path.join(repo, "package.json"), "{}");
  const native = path.join(root, "native.mjs");
  fs.writeFileSync(native, `#!/usr/bin/env node
const argv = process.argv.slice(2);
if (argv[0] !== "derive") process.exit(2);
const workspace = argv[argv.indexOf("--workspace") + 1];
process.stdout.write(JSON.stringify({
  schema: "carrick.derive/0", workspace, repos_detected_by: "single repository",
  repos_added: [], repos_excluded: [], missing: [], parent_proposal: null,
  repos: [{ path: workspace, reason: "single repository", services: [{ serviceName: "api" }], config: null, warnings: [],
    not_installed: [{ service: "api", directory: ".", command: "npm install" }] }],
}));
`);
  fs.chmodSync(native, 0o755);
  const restoreEnv = withEnv({
    CARRICK_BIN: native,
    CARRICK_TOKEN: "test-token",
    XDG_CONFIG_HOME: path.join(root, "config"),
    HOME: path.join(root, "home"),
    PATH: `${fakeClaude(root)}${path.delimiter}${process.env["PATH"] ?? ""}`,
  });
  const fetchBefore = globalThis.fetch;
  const asked: string[] = [];
  // Where the repo is: nowhere at the first read, and by the time the proposal
  // is accepted the App has connected it.
  let where: "unconnected" | "staged" | "default" | "acme" = "unconnected";
  const installed = (): void => {
    if (where === "unconnected") where = claims === "before-1359" ? "default" : "staged";
  };
  let reads = 0;
  globalThis.fetch = (async (_input: string, init: { body: string }) => {
    const body = JSON.parse(String(init.body));
    asked.push(body.action);
    // The App install lands after the first read, while the questions are
    // being answered.
    if (body.action !== "resolve-repos" || reads > 0) installed();
    if (body.action === "resolve-repos") {
      // The network goes away after the questions are answered.
      if (claims === "read-fails" && reads > 0) throw new TypeError("fetch failed");
      reads += 1;
      const connected = where === "default" || where === "acme";
      return Response.json({
        schema: "carrick.resolve-repos/0",
        workspace: { slug: "acme", billing_tier: "free", installed: where !== "unconnected" },
        allowance_sentence: null,
        repos: [connected
          ? { full_name: "acme/acme-api", connected: true, project_id: "p1", project_slug: where, services: [] }
          : { full_name: "acme/acme-api", connected: false }],
        project_repos: connected ? [{ project_slug: where, repos: ["acme/acme-api"] }] : [],
      });
    }
    if (body.action === "list-projects") {
      return Response.json({ schema: "carrick.list-projects/0", projects: [] });
    }
    if (body.action === "create-project") {
      const took = where === "staged" ? 1 : 0;
      if (took > 0) where = "acme";
      return Response.json({
        schema: "carrick.create-project/0",
        project: claims !== "before-1359"
          ? { slug: body.slug, name: body.name, archived: false, repo_count: took }
          : { slug: body.slug, name: body.name, archived: false },
      });
    }
    if (body.action === "assign-repos") {
      const moved = where !== body.project;
      where = "acme";
      return Response.json({
        schema: "carrick.assign-repos/0", project_slug: body.project,
        repos: body.repos.map((name: string) => ({ full_name: name, assigned: true, moved, project_slug: body.project, reason: null })),
      });
    }
    throw new Error(`unexpected action ${body.action}`);
  }) as unknown as typeof fetch;
  return {
    repo,
    asked,
    restore: () => {
      restoreEnv();
      globalThis.fetch = fetchBefore;
      fs.rmSync(root, { recursive: true, force: true });
    },
  };
}

// carrick#1489 parts 1 to 5 in one first run: the pick list, a project by
// name, the summary the yes is given to, a connection read after the
// questions rather than before them, and the install in the agent's
// instruction.
for (const claims of ["adopts", "before-1359"] as const) {
  test(`a first run creates the project by name and ends in three parts (${claims})`, async () => {
    const fixture = firstRunRepo(claims);
    try {
      const questions: string[] = [];
      const out = recordingOutput({
        pick: async (question, options) => {
          questions.push(`${question} [${options.map((option) => option.label).join(" | ")}]`);
          return options.find((option) => option.label === "New project")!.value;
        },
        ask: async (question) => {
          questions.push(question);
          return "Acme";
        },
        confirm: async (question) => {
          questions.push(question);
          // Never a global install from a test.
          return !question.startsWith("Install carrick");
        },
      });
      assert.equal(await initWith([fixture.repo], out, true), 0, out.lines.join("\n"));
      // The project question names the repo, and the project is not
      // confirmed at a question of its own: the one "Go ahead?" covers the
      // create, the App and the files (carrick#1512).
      assert.deepEqual(questions.slice(0, 3), [
        "Which project should acme-api be in? [New project | Choose on the dashboard]",
        "Project name",
        "Go ahead?",
      ]);
      const said = out.lines.join("\n");
      // Part 2: everything the run will do, a line each, under "Next:".
      assert.ok(
        out.lines.includes(
          [
            " Next:",
            "  Create project Acme with acme-api",
            "  Open GitHub to install Carrick on acme-api",
            // Broken inside the gutter, where the mock breaks it.
            "  Add Carrick's hooks and skills to .claude/, .agents/ and .codex/,",
            "  create .carrick/, and add Carrick's MCP server to Claude Code",
          ].join("\n"),
        ),
        said,
      );
      // Nothing was created before that answer.
      assert.ok(fixture.asked.indexOf("create-project") > fixture.asked.indexOf("list-projects"), fixture.asked.join(","));
      // Part 3: the connection is read again after the write was accepted, so
      // a repo the App connected meanwhile is not sent to the grant page
      // again, and nothing waits. The App line was said once, in the list,
      // and a terminal opens the page rather than printing its address.
      assert.equal(out.lines.filter((line) => line.includes("/connect")).length, 0, said);
      assert.equal(out.lines.filter((line) => /install Carrick on|Install the Carrick GitHub App/.test(line)).length, 1, said);
      assert.doesNotMatch(said, /Waiting|currently in project|Moved acme|A workspace owner/);
      if (claims === "adopts") {
        // The first project took the staged repo, so there is nothing to move.
        assert.ok(!fixture.asked.includes("assign-repos"), fixture.asked.join(","));
      } else {
        // Before carrick-cloud#1359: the grant put it in a default project,
        // and init moved it out without a line about it.
        assert.ok(fixture.asked.includes("assign-repos"), fixture.asked.join(","));
      }
      // No line of its own for the create: the list said it (carrick#1512).
      assert.doesNotMatch(said, /Created project/);
      // Part 4: one line for what is set up — the project and its repos, as
      // David's mock has it — the to-dos, then the prompt; part 5: the
      // install leads it.
      assert.ok(out.lines.includes("◇ Acme (acme): acme-api"), said);
      assert.equal(
        out.lines.at(-1),
        [
          "Next: paste this into a new agent session",
          "First install dependencies: run `npm install`.",
          scaffoldFor("acme/acme-api"),
        ].join("\n"),
      );
      assert.doesNotMatch(said, /slug|No agent client|Docs:|Restart Claude Code/);
    } finally {
      fixture.restore();
    }
  });
}

// carrick#1489 review: the read after the yes used to run before anything was
// written, so a network error there exited 1 with no proposal and no hooks.
// The local files come first now, and a failed read is one line saying what
// is left.
test("a workspace read that fails after the yes leaves the setup written and exits 0", async () => {
  const fixture = firstRunRepo("read-fails");
  try {
    const out = recordingOutput({
      pick: async (_question, options) => options.find((option) => option.label === "New project")!.value,
      ask: async () => "Acme",
      confirm: async (question) => !question.startsWith("Install carrick"),
    });
    assert.equal(await initWith([fixture.repo], out, true), 0, out.lines.join("\n"));
    assert.ok(fs.existsSync(path.join(fixture.repo, PROPOSAL_FILE)));
    assert.ok(fs.existsSync(path.join(fixture.repo, CODEX_HOOKS_FILE)));
    assert.ok(
      out.lines.includes(
        "▲ Could not reach Carrick to verify the workspace. Check the connection and retry. The files here are written; run carrick init again to verify the connection and the project.",
      ),
      out.lines.join("\n"),
    );
    assert.equal(out.lines.at(-1)?.startsWith("Next: paste this into a new agent session"), true, out.lines.join("\n"));
  } finally {
    fixture.restore();
  }
});

test("the plain rendering is one line per thing, with no colour and no box", () => {
  const written: string[] = [];
  const out = plainOutput((text) => void written.push(text));
  out.done("Repo acme/api connected");
  out.warn("Hosted index is older than this CLI");
  out.refuse("Hosted index could not be read into .carrick/: no reason");
  out.say("Connect repositories in your browser: https://app.carrick.tools/repos");
  out.note("Next: paste this to your agent", [scaffoldFor("acme/api")]);
  assert.deepEqual(written.join("").split("\n"), [
    "◇ Repo acme/api connected",
    "▲ Hosted index is older than this CLI",
    "■ Hosted index could not be read into .carrick/: no reason",
    "Connect repositories in your browser: https://app.carrick.tools/repos",
    "",
    "Next: paste this to your agent",
    `  ${scaffoldFor("acme/api")}`,
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
    hostedReport({ kind: "downloaded", services: 2, reread: true }),
  );
  // Rendered, then read as text: the colours and the cursor are the
  // terminal's business and the markers are the contract.
  const plain = drawn.join("").replace(/\[[0-9;?]*[A-Za-z]/g, "");
  const lines = plain
    .split("\n")
    .map((line) => line.replace(/\s+/g, " ").trim())
    .filter((line) => line.length > 0 && line !== "│");
  assert.deepEqual(lines, [
    "▲ .carrick/ holds 3 services as this machine read them; the hosted rows could not be replayed onto this checkout",
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

// carrick#1489: the project question is one row out of several, so the plain
// rendering numbers them and takes one number.
test("the numbered pick list takes one number, and refuses anything else", async () => {
  const written: string[] = [];
  const input = new PassThrough();
  const out = plainOutput((text) => void written.push(text), { input, output: new PassThrough() });
  const options = [
    { value: "project:payments", label: "Payments (payments)", hint: "1 repo" },
    { value: "new", label: "New project" },
  ];
  const answer = out.pick("Which project should these repos be in?", options);
  setImmediate(() => {
    input.write("3\n");
    setImmediate(() => input.write("2\n"));
  });
  assert.equal(await answer, "new");
  assert.deepEqual(written, [
    "Which project should these repos be in?\n",
    "  1. Payments (payments)  (1 repo)\n",
    "  2. New project\n",
    '■ Not a number between 1 and 2: "3"\n',
  ]);
});

// The four index states, as lines rather than as tags. Each one says what this
// machine now holds; the two with something to do about it carry the marker
// that says so, and the one that produced nothing is a refusal (carrick#1020,
// carrick#1026).
test("every hosted outcome is one line, and only an actionable one is a warning", () => {
  assert.deepEqual(hostedReport({ kind: "downloaded", services: 2, reread: true }), {
    kind: "done",
    text: "Hosted index for 2 services read into .carrick/",
  });
  assert.deepEqual(
    hostedReport({ kind: "downloaded", services: 1, reread: true }).text,
    "Hosted index for 1 service read into .carrick/",
  );
  // With the time the step took, which is what a wait of minutes ends on.
  assert.equal(
    hostedReport({ kind: "downloaded", services: 15, reread: true }, 184).text,
    "Hosted index for 15 services read into .carrick/ in 3m4s",
  );
  // And the same rows, on a run that spent none of those minutes: the reader
  // is told which of the two they got (carrick#1373).
  const kept = hostedReport({ kind: "downloaded", services: 15, reread: false }, 0.4);
  assert.equal(kept.kind, "done");
  assert.equal(kept.text, ".carrick/ already holds the hosted index for 15 services: nothing was re-read");
  assert.doesNotMatch(kept.text, /in 0/);
  const older = hostedReport({ kind: "version_mismatch", services: 2, reread: true });
  assert.equal(older.kind, "warn");
  assert.match(older.text, /run `carrick index --detach` once from main/);
  // And never a downgrade: `CACHE_VERSION` moves most weeks (carrick#1012).
  assert.doesNotMatch(older.text, /npm i -g carrick@/);
  const local = hostedReport({ kind: "local_only", services: 3, state: "commit_missing", reread: true });
  assert.equal(local.kind, "warn");
  assert.match(local.text, /\.carrick\/ holds 3 services as this machine read them; the commit/);
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
  assert.deepEqual(outcome, { kind: "downloaded", services: 1, reread: false });
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
  assert.deepEqual(outcome, { kind: "downloaded", services: 1, reread: true });
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

// carrick#1489 part 5: the agent's first `carrick index` was refused because
// neither repo had its dependencies installed. The scanner states which
// services a scan would refuse over, and init puts the install in front of the
// instruction, each command where it runs, from the workspace the agent is in.
test("the installs a first scan would refuse over lead the agent's instruction", () => {
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
    installSentence(commands, "agent"),
    "First install dependencies: run `npm install` in acme-api and `pnpm install` in acme-app/apps/web.",
  );
  assert.equal(
    installSentence(commands, "reader"),
    "Dependencies are not installed. Run `npm install` in acme-api and `pnpm install` in acme-app/apps/web before the next scan.",
  );
  assert.equal(installSentence([], "agent"), null);
});

// carrick#1489 part 4: nine blocks closed a first run. What is set up is one
// line; what is left to do is a line each, and only when there is something.
test("what is set up is one line: the project and the repos in it", () => {
  // The line David's mock settled on (carrick#1512), in folder order.
  assert.equal(summaryLine("Shop (shop)", ["shop-app", "shop-api"]), "Shop (shop): shop-api, shop-app");
  assert.equal(summaryLine(null, ["shop-app"]), "No project: shop-app");
  assert.equal(
    summaryLine("Shop (shop)", Array.from({ length: 12 }, (_, index) => `repo-${String(index).padStart(2, "0")}`)),
    "Shop (shop): repo-00, repo-01, repo-02, repo-03, repo-04, repo-05, repo-06, repo-07, repo-08, repo-09 and 2 more",
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

// What the picker opens on. A default is a claim about somebody's folder, so
// only a repo the server already says belongs here carries one: connected, and
// in the project most of this folder's connected repos are in (carrick#1365).
test("the picker preselects the connected repos of the majority project, and says why the rest are out", () => {
  const repo = (name: string | null, dir: string) => ({ path: `/w/${dir}`, name, remote: null, problem: null });
  const candidates = [repo("acme/api", "api"), repo("acme/web", "web"), repo("acme/demo", "demo"), repo("acme/new", "new"), repo(null, "local")];
  const connected = (full_name: string, project_slug: string) =>
    ({ full_name, connected: true as const, project_id: project_slug, project_slug, services: [] });
  const identity = {
    schema: "carrick.resolve-repos/0" as const,
    workspace: { slug: "acme", billing_tier: "free" as const, installed: true },
    allowance_sentence: null,
    repos: [
      connected("acme/api", "payments"),
      connected("ACME/web", "payments"),
      connected("acme/demo", "demo-repos"),
      { full_name: "acme/new", connected: false as const },
    ],
    project_repos: [],
  };
  const chosen = preselectedRepos(candidates, identity, (slug) => (slug === "demo-repos" ? "Demo Repos" : slug));
  assert.deepEqual(chosen.keep, ["/w/api", "/w/web"]);
  assert.deepEqual([...chosen.reasons], [
    ["/w/new", "not connected"],
    ["/w/local", "no GitHub identity"],
    ["/w/demo", "in Demo Repos"],
  ]);

  // A tie is not a majority, and no read is not "not connected": both open on
  // nothing, and the second gives no reason it has no source for.
  const tie = { ...identity, repos: [connected("acme/api", "payments"), connected("acme/demo", "demo-repos")] };
  assert.deepEqual(preselectedRepos(candidates, tie).keep, []);
  const unread = preselectedRepos(candidates, null);
  assert.deepEqual(unread.keep, []);
  assert.deepEqual([...unread.reasons], [["/w/local", "no GitHub identity"]]);
});

// The closing instruction is about the repos that still need it. A repo with
// its config and its workflow is already set up, and telling an agent to
// scaffold it again is what the run did on a folder where every repo was
// (carrick#1365).
test("only a repo missing its config or its workflow is sent to the scaffold tool", () => {
  const repo = (name: string) => ({ path: `/w/${name}`, name: `acme/${name}`, remote: null, problem: null });
  const repos = ["api", "web", "jobs", "docs", "site"].map(repo);
  const present = new Set([
    "/w/api/carrick.json",
    `/w/api/${TEMPLATE_PATHS.workflow}`,
    "/w/web/carrick.json",
  ]);
  const owed = reposToScaffold(repos, (target) => present.has(target));
  assert.deepEqual(owed.map((entry) => entry.name), ["acme/web", "acme/jobs", "acme/docs", "acme/site"]);
  assert.deepEqual(reposToScaffold([repos[0]!], (target) => present.has(target)), []);

  assert.equal(
    scaffoldSentence([repos[1]!]),
    "Run the carrick scaffold tool for acme/web, passing its owner/repo as `repo`, and follow what it returns.",
  );
  assert.equal(
    scaffoldSentence(owed),
    "Run the carrick scaffold tool for acme/web, acme/jobs, acme/docs and 1 more, once each, passing that repo's owner/repo as `repo`, and follow what it returns.",
  );
});

// A file under the home directory is outside the workspace the proposal is
// about, so it has its own answer: the terminal's, or `--mcp`. `--yes` and a
// run with no terminal are not one (carrick#1365).
test("an editor file is written only for an editor somebody named", async () => {
  const offered = [
    { name: "Cursor", file: "/home/dev/.cursor/mcp.json", installed: true },
    { name: "Windsurf", file: "/home/dev/.codeium/windsurf/mcp_config.json", installed: false },
  ];
  const asked: { initial: string[]; required: boolean }[] = [];
  const out = recordingOutput({
    choose: async (_question, _noun, options, config) => {
      asked.push({ initial: [...config.initial], required: config.required });
      return options.map((option) => option.value);
    },
  });
  const none = { editors: [], assumeYes: false, interactive: false };
  assert.deepEqual(await chooseEditors(offered, none, out), []);
  assert.deepEqual(await chooseEditors(offered, { ...none, interactive: true, assumeYes: true }, out), []);
  assert.deepEqual(asked, []);

  // The flag names them, in the casing init prints, once each.
  assert.deepEqual(await chooseEditors(offered, { ...none, editors: ["cursor", "Cursor"] }, out), ["Cursor"]);
  await assert.rejects(
    chooseEditors(offered, { ...none, editors: ["Zed"] }, out),
    /--mcp Zed names no editor configured on this machine\. The ones here are: Cursor, Windsurf\./,
  );

  // The terminal asks, ticking only what is detected, and takes none for an answer.
  assert.deepEqual(await chooseEditors(offered, { ...none, interactive: true }, out), ["Cursor", "Windsurf"]);
  assert.deepEqual(asked, [{ initial: ["Cursor"], required: false }]);
  assert.deepEqual(await chooseEditors([], { ...none, interactive: true }, out), []);

  // The files an answer covers are named by path in the list the question is
  // about (carrick#1489), the home directory as ~, and as what happens to
  // them: every writer merges, and a list of folders read as though it
  // replaced them (carrick#1512).
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

// carrick#1489 part 2: the summary named each repo twice, restated the
// project the reader had just confirmed, and hid the one browser step behind
// "once the browser connects them". One block per project, each repo once.
test("the list Go ahead is asked about names the project, the App and the files, once each", () => {
  const writes = "Add Carrick's hooks and skills to .claude/, .agents/ and .codex/, create .carrick/, and add Carrick's MCP server to Claude Code";
  const url = "https://app.carrick.tools/w/acme/connect";
  // The first run of the smoke case: a new project, neither repo connected.
  assert.deepEqual(
    nextLines({
      create: true,
      project: "Shop",
      repos: ["shop-app", "shop-api"],
      joining: ["shop-app", "shop-api"],
      moving: [],
      connect: connectItem(["shop-app", "shop-api"], 2, true, url),
      writes,
    }),
    [
      "Create project Shop with shop-app and shop-api",
      "Open GitHub to install Carrick on both repos",
      writes,
    ],
  );
  // An existing project: the repos that join it, and a move from where one is
  // now, which is still asked about on its own afterwards (carrick#1338).
  assert.deepEqual(
    nextLines({
      create: false,
      project: "Shop (shop)",
      repos: ["shop-app", "shop-api"],
      joining: ["shop-app"],
      moving: [{ repo: "shop-api", from: "Default (default)" }],
      connect: connectItem(["shop-app"], 2, true, url),
      writes,
    }),
    [
      "Add shop-app to project Shop (shop)",
      "Move shop-api from Default (default) into Shop (shop)",
      "Open GitHub to install Carrick on shop-app",
      writes,
    ],
  );
  // Nothing to create, connect or move: only the files.
  assert.deepEqual(
    nextLines({ create: false, project: "Shop (shop)", repos: ["shop-app"], joining: [], moving: [], connect: null, writes }),
    [writes],
  );
  // Without a terminal nothing can open the page, so the line carries it.
  assert.equal(connectItem(["shop-app"], 1, false, url), `Install the Carrick GitHub App on shop-app: ${url}`);
  assert.equal(connectItem(["a", "b", "c"], 3, true, url), "Open GitHub to install Carrick on all 3 repos");
  // Printed inside the gutter: a long line is broken between words, the way
  // the mock breaks it, rather than left for the terminal to wrap under the
  // gutter (carrick#1512).
  assert.equal(
    nextBlock(["Create project Shop with shop-app and shop-api", writes]),
    [
      "Next:",
      "  Create project Shop with shop-app and shop-api",
      "  Add Carrick's hooks and skills to .claude/, .agents/ and .codex/,",
      "  create .carrick/, and add Carrick's MCP server to Claude Code",
    ].join("\n"),
  );
  assert.ok(wrapped(writes).every((line) => line.length <= 68));
});

/**
 * A folder of repos the way the smoke run held them (carrick#1512): two
 * JavaScript repos side by side, a third holding nothing but `.git`, and a
 * derive that answers the way the scanner does — from inside a repo, that
 * repo and the folder above it; from the folder, its repos.
 */
function siblingFolder(options: {
  /** Extra JavaScript repos beside the two, for the long-list case. */
  extra?: number;
  /** What the workspace read says the signed-in user is. */
  role?: string;
  /** Whether the App lands on the repos while the questions are answered. */
  connects?: boolean;
  /** Repos the project holds beyond the ones this run asks about. */
  alsoInProject?: string[];
  /** A JavaScript repo beside the others whose clone has no remote. */
  unnamed?: boolean;
}): { folder: string; api: string; app: string; asked: string[]; opened: () => string[]; restore: () => void } {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-init-siblings-")));
  const folder = path.join(root, "shop");
  const repo = (name: string, manifest: boolean): string => {
    const dir = path.join(folder, name);
    fs.mkdirSync(dir, { recursive: true });
    execFileSync("git", ["init", "-q", dir]);
    execFileSync("git", ["-C", dir, "remote", "add", "origin", `https://github.com/example-org/${name}.git`]);
    if (manifest) fs.writeFileSync(path.join(dir, "package.json"), "{}\n");
    return dir;
  };
  const api = repo("shop-api", true);
  const app = repo("shop-app", true);
  repo("tools-py", false);
  for (let index = 0; index < (options.extra ?? 0); index += 1) repo(`lib-${String(index).padStart(2, "0")}`, true);
  if (options.unnamed === true) execFileSync("git", ["-C", repo("notes", true), "remote", "remove", "origin"]);
  const native = path.join(root, "native.mjs");
  fs.writeFileSync(native, `#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
const argv = process.argv.slice(2);
if (argv[0] !== "derive") process.exit(2);
const workspace = fs.realpathSync(argv[argv.indexOf("--workspace") + 1]);
const markers = [".git", "package.json", "carrick.json", "deno.json", "deno.jsonc", "pnpm-workspace.yaml"];
const marked = (dir) => markers.some((name) => fs.existsSync(path.join(dir, name)));
const children = (dir) => fs.readdirSync(dir, { withFileTypes: true })
  .filter((entry) => entry.isDirectory() && !entry.name.startsWith("."))
  .map((entry) => path.join(dir, entry.name)).filter(marked).sort();
let exclude = [];
try { exclude = JSON.parse(fs.readFileSync(path.join(workspace, "carrick-workspace.json"), "utf8")).exclude ?? []; } catch {}
const entry = (repo) => ({ path: repo, reason: "test", services: [{ serviceName: path.basename(repo) }], config: null, warnings: [], not_installed: [] });
const base = { schema: "carrick.derive/0", workspace, repos_added: [], repos_excluded: exclude, missing: [] };
const inside = children(workspace);
const doc = inside.length > 0
  ? { ...base, repos_detected_by: "sibling repositories", parent_proposal: null,
      repos: inside.filter((repo) => !exclude.includes(path.basename(repo))).map(entry) }
  : { ...base, repos_detected_by: "single repository",
      parent_proposal: { directory: path.dirname(workspace), repos: children(path.dirname(workspace)) },
      repos: [entry(workspace)] };
process.stdout.write(JSON.stringify(doc));
`);
  fs.chmodSync(native, 0o755);
  const bin = fakeClaude(root);
  // The browser, so a wait never opens a real one: `open` on macOS, xdg-open
  // elsewhere, each logging the address it was handed.
  for (const name of ["open", "xdg-open"]) {
    fs.writeFileSync(path.join(bin, name), `#!/bin/sh\necho "$1" >> ${JSON.stringify(path.join(root, "opened.log"))}\nexit 0\n`);
    fs.chmodSync(path.join(bin, name), 0o755);
  }
  const restoreEnv = withEnv({
    CARRICK_BIN: native,
    CARRICK_TOKEN: "test-token",
    XDG_CONFIG_HOME: path.join(root, "config"),
    HOME: path.join(root, "home"),
    PATH: `${bin}${path.delimiter}${process.env["PATH"] ?? ""}`,
  });
  const fetchBefore = globalThis.fetch;
  const asked: string[] = [];
  let reads = 0;
  let placed: string | null = null;
  const created: string[] = [];
  globalThis.fetch = (async (_input: string, init: { body: string }) => {
    const body = JSON.parse(String(init.body));
    asked.push(`${body.action}${Array.isArray(body.repos) ? ` ${body.repos.join(",")}` : ""}`);
    if (body.action === "resolve-repos") {
      const connected = options.connects !== false && reads > 0 && placed !== null;
      reads += 1;
      return Response.json({
        schema: "carrick.resolve-repos/0",
        workspace: {
          slug: "example-org",
          billing_tier: "free",
          installed: connected,
          ...(options.role === undefined ? {} : { role: options.role }),
        },
        allowance_sentence: null,
        repos: body.repos.map((name: string) =>
          connected
            ? { full_name: name, connected: true, project_id: "p1", project_slug: placed, services: [] }
            : { full_name: name, connected: false }),
        project_repos: connected ? [{ project_slug: placed, repos: [...body.repos, ...(options.alsoInProject ?? [])] }] : [],
      });
    }
    if (body.action === "list-projects") {
      return Response.json({
        schema: "carrick.list-projects/0",
        projects: created.map((slug) => ({ slug, name: slug, archived: false, repo_count: 0 })),
      });
    }
    if (body.action === "create-project") {
      created.push(body.slug);
      placed = body.slug;
      return Response.json({
        schema: "carrick.create-project/0",
        project: { slug: body.slug, name: body.name, archived: false, repo_count: 0 },
      });
    }
    if (body.action === "assign-repos") {
      placed = body.project;
      return Response.json({
        schema: "carrick.assign-repos/0", project_slug: body.project,
        repos: body.repos.map((name: string) => ({ full_name: name, assigned: true, moved: false, project_slug: body.project, reason: null })),
      });
    }
    throw new Error(`unexpected action ${body.action}`);
  }) as unknown as typeof fetch;
  return {
    folder,
    api,
    app,
    asked,
    opened: () =>
      fs.existsSync(path.join(root, "opened.log"))
        ? fs.readFileSync(path.join(root, "opened.log"), "utf8").split("\n").filter((line) => line !== "")
        : [],
    restore: () => {
      restoreEnv();
      globalThis.fetch = fetchBefore;
      fs.rmSync(root, { recursive: true, force: true });
    },
  };
}

/** A terminal that ticks these rows, names a new project, and says yes. */
function siblingTerminal(tick: (rows: Choice[]) => string[]): InitOutput & {
  lines: string[];
  questions: Array<{ question: string; rows?: Choice[]; initial?: string[] }>;
} {
  const questions: Array<{ question: string; rows?: Choice[]; initial?: string[] }> = [];
  const out = recordingOutput({
    choose: async (question, _noun, rows, config) => {
      questions.push({ question, rows, initial: config.initial });
      return tick(rows);
    },
    pick: async (question, options) => {
      questions.push({ question });
      return options.find((option) => option.label === "New project")!.value;
    },
    ask: async (question) => {
      questions.push({ question });
      return "Shop";
    },
    confirm: async (question) => {
      questions.push({ question });
      return !question.startsWith("Install carrick");
    },
  });
  return Object.assign(out, { questions });
}

function gitStatus(dir: string): string {
  return execFileSync("git", ["-C", dir, "status", "--porcelain", "--untracked-files=all"], { encoding: "utf8" });
}

// carrick#1512. Run inside one repo of a folder, init named the folder above
// and told the reader to start again there. It asks instead: this repo
// ticked, the JavaScript repos beside it unticked and named by their GitHub
// repo, and a yes to any of them carries on as the folder run would.
test("inside one repo, init asks which repos beside it belong with it, and a yes sets up the folder", posixNativeFixture, async () => {
  const fixture = siblingFolder({});
  try {
    const out = siblingTerminal((rows) => rows.map((row) => row.value));
    assert.equal(await initWith([fixture.app], out, true), 0, out.lines.join("\n"));
    const said = out.lines.join("\n");
    const first = out.questions[0]!;
    assert.equal(first.question, `${fixture.folder} holds other repos. Which belong to the same system as shop-app?`);
    // This repo first and ticked; the others unticked; a folder holding only
    // `.git` is not a candidate for "the same system".
    assert.deepEqual(first.rows, [
      { value: fixture.app, label: "shop-app (this repo)", hint: "example-org/shop-app" },
      { value: fixture.api, label: "shop-api", hint: "example-org/shop-api" },
    ]);
    assert.deepEqual(first.initial, [fixture.app]);
    // The project question names both repos, this one first, and nothing is
    // asked to be confirmed before the one "Go ahead?".
    // The first three after it: a machine with no global carrick is then
    // offered one, which is a question about the machine, not this setup.
    assert.deepEqual(out.questions.slice(1, 4).map((entry) => entry.question), [
      "Which project should shop-app and shop-api be in?",
      "Project name",
      "Go ahead?",
    ]);
    // The list names where the hooks and skills go: the folder, and each
    // repo in it (carrick#1512, option A as ruled).
    assert.ok(
      out.lines.includes(
        ` ${nextBlock([
          "Create project Shop with shop-app and shop-api",
          "Open GitHub to install Carrick on both repos",
          `Add Carrick's hooks and skills to .claude/, .agents/ and .codex/ in ${fixture.folder} and in each repo, create .carrick/ and carrick-workspace.json, and add Carrick's MCP server to Claude Code`,
        ])}`,
      ),
      said,
    );
    // No line for the create; the closing line is the project and its repos.
    assert.doesNotMatch(said, /Created project/);
    assert.ok(out.lines.includes("◇ Shop (shop): shop-api, shop-app"), said);
    // Not told to run it again anywhere.
    assert.doesNotMatch(said, /Run carrick init \.\.|parent folder/);
    // Nothing was created before the answer: the create is the first write.
    assert.equal(fixture.asked.findIndex((line) => line.startsWith("create-project")) > 0, true, fixture.asked.join("\n"));
    // Set up as the folder: the proposal and the selection in the folder, the
    // repo nobody offered left out of it.
    assert.ok(fs.existsSync(path.join(fixture.folder, PROPOSAL_FILE)));
    assert.equal(fs.existsSync(path.join(fixture.app, ".carrick")), false);
    assert.deepEqual(JSON.parse(fs.readFileSync(path.join(fixture.folder, WORKSPACE_FILE), "utf8")).exclude, ["tools-py"]);
    // Option A: each repo holds its own hooks and skills, and git sees none
    // of them.
    for (const repo of [fixture.api, fixture.app]) {
      assert.ok(fs.existsSync(path.join(repo, ".claude", "settings.local.json")), repo);
      assert.ok(fs.existsSync(path.join(repo, CODEX_HOOKS_FILE)), repo);
      for (const skill of taskSkillPaths()) assert.ok(fs.existsSync(path.join(repo, skill)), skill);
      assert.equal(gitStatus(repo), "?? package.json\n", repo);
    }
    // The agent is told where the proposal is, because it is not where the
    // reader started.
    assert.equal(
      out.lines.at(-1),
      [
        "Next: paste this into a new agent session",
        scaffoldSentence([
          { path: fixture.app, name: "example-org/shop-app", remote: null, problem: null },
          { path: fixture.api, name: "example-org/shop-api", remote: null, problem: null },
        ]),
        `The init folder is ${fixture.folder}.`,
      ].join("\n"),
    );
    assert.doesNotMatch(said, /Restart Claude Code|not on this machine/);
  } finally {
    fixture.restore();
  }
});

// carrick#1512: left unticked, the repo beside this one is named nowhere, and
// a project that also holds it (every repo the App was granted, until
// carrick-cloud#1403) does not call it missing from this machine.
test("with only this repo ticked, every line names it alone and a sibling is never 'not on this machine'", posixNativeFixture, async () => {
  const fixture = siblingFolder({ alsoInProject: ["example-org/shop-api", "example-org/billing"] });
  try {
    const out = siblingTerminal((rows) => [rows[0]!.value]);
    assert.equal(await initWith([fixture.app], out, true), 0, out.lines.join("\n"));
    const said = out.lines.join("\n");
    assert.deepEqual(out.questions.slice(1, 4).map((entry) => entry.question), [
      "Which project should shop-app be in?",
      "Project name",
      "Go ahead?",
    ]);
    assert.ok(said.includes("  Create project Shop with shop-app\n  Open GitHub to install Carrick on shop-app\n"), said);
    // The repo that really is elsewhere is still named; the one beside this
    // one is not.
    assert.ok(out.lines.includes("▲ Also in this project, not on this machine: example-org/billing."), said);
    assert.doesNotMatch(said, /shop-api/);
    // Set up where it started, as a single repo, with no copy anywhere else.
    assert.ok(fs.existsSync(path.join(fixture.app, PROPOSAL_FILE)));
    assert.equal(fs.existsSync(path.join(fixture.folder, ".carrick")), false);
    assert.equal(fs.existsSync(path.join(fixture.api, ".claude")), false);
    assert.doesNotMatch(said, /The init folder is/);
  } finally {
    fixture.restore();
  }
});

// carrick#1512: more than fifteen repos beside this one is not a list to tick
// through, and a run with no terminal cannot ask. Both say one line and set
// up this repo.
test("too many repos beside this one, or no terminal, is one line and this repo alone", posixNativeFixture, async () => {
  const line = "Setting up shop-app only. To set up several repos as one system, run carrick init in the folder that holds them.";
  const many = siblingFolder({ extra: 15 });
  try {
    const out = siblingTerminal(() => {
      throw new Error("no list above fifteen repos");
    });
    assert.equal(await initWith([many.app], out, true), 0, out.lines.join("\n"));
    assert.ok(out.lines.includes(` ${line}`), out.lines.join("\n"));
    assert.equal(out.questions[0]!.question, "Which project should shop-app be in?");
  } finally {
    many.restore();
  }
  const few = siblingFolder({});
  try {
    const out = siblingTerminal(() => {
      throw new Error("no terminal to ask in");
    });
    assert.equal(await initWith(["--yes", few.app], out, false), 0, out.lines.join("\n"));
    assert.ok(out.lines.includes(` ${line}`), out.lines.join("\n"));
    assert.equal(fs.existsSync(path.join(few.folder, ".carrick")), false);
  } finally {
    few.restore();
  }
});

// carrick#1512 and carrick-cloud#1426: the owner-or-admin line is for a
// member, and the GitHub App line is said once, in the list. A terminal opens
// the page itself.
test("the wait on the App says who can end it only to a member, and the App line is said once", posixNativeFixture, async () => {
  for (const role of ["member", "owner", undefined]) {
    const fixture = siblingFolder({ connects: false, ...(role === undefined ? {} : { role }) });
    try {
      const out = siblingTerminal((rows) => [rows[0]!.value]);
      // The wait is stopped the way a reader stops it.
      out.step = async (_label, work, report) => {
        setImmediate(() => process.emit("SIGINT"));
        const value = await work(() => {});
        out.lines.push(report(value).text);
        return value;
      };
      assert.equal(await initWith([fixture.app], out, true), 0, out.lines.join("\n"));
      const said = out.lines.join("\n");
      assert.equal(said.includes(adminWait()), role === "member", `${String(role)}: ${said}`);
      assert.deepEqual(fixture.opened(), ["https://app.carrick.tools/w/example-org/connect"]);
      // Once in the list; and, the wait stopped with the grant still to do,
      // once as what is left, with the page to finish it on.
      assert.equal(out.lines.filter((entry) => entry.includes("install Carrick on shop-app")).length, 1, said);
      assert.equal(
        out.lines.filter((entry) => entry.includes("Install the Carrick GitHub App on it: https://app.carrick.tools/w/example-org/connect")).length,
        1,
        said,
      );
    } finally {
      fixture.restore();
    }
  }
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

// carrick#1512: nothing is created, connected or written before the one
// "Go ahead?". A no to it, after a sibling was ticked and a new project named,
// leaves the folder, both repos and Carrick as they were.
test("a no to Go ahead creates no project and writes nothing, in the folder or the repos", posixNativeFixture, async () => {
  const fixture = siblingFolder({});
  try {
    const out = siblingTerminal((rows) => rows.map((row) => row.value));
    out.confirm = async (question) => {
      out.questions.push({ question });
      return false;
    };
    assert.equal(await initWith([fixture.app], out, true), 0, out.lines.join("\n"));
    assert.equal(out.questions.at(-1)?.question, "Go ahead?");
    assert.ok(out.lines.includes(`■ ${NOTHING_WRITTEN}`), out.lines.join("\n"));
    assert.deepEqual(
      fixture.asked.filter((line) => !line.startsWith("resolve-repos") && line !== "list-projects"),
      [],
    );
    assert.deepEqual(fs.readdirSync(fixture.folder).sort(), ["shop-api", "shop-app", "tools-py"]);
    for (const repo of [fixture.api, fixture.app]) {
      assert.deepEqual(fs.readdirSync(repo).sort(), [".git", "package.json"], repo);
    }
  } finally {
    fixture.restore();
  }
});

// carrick#1512 review R4, as ruled: a repo the folder above has already set
// up is set up from that folder again, with one line saying so, rather than
// offered on its own beside it.
test("inside a repo the folder above already set up, init sets up the folder again and says so", posixNativeFixture, async () => {
  const fixture = siblingFolder({});
  try {
    fs.mkdirSync(path.join(fixture.folder, ".carrick"), { recursive: true });
    fs.writeFileSync(
      path.join(fixture.folder, PROPOSAL_FILE),
      JSON.stringify({ schema: "carrick.derive/0", repos: [{ path: fixture.api }, { path: fixture.app }] }),
    );
    const out = siblingTerminal((rows) => rows.filter((row) => path.basename(row.value).startsWith("shop-")).map((row) => row.value));
    assert.equal(await initWith([fixture.app], out, true), 0, out.lines.join("\n"));
    assert.equal(out.lines[0], ` shop-app is set up with the repos in ${fixture.folder}, so this run sets up ${fixture.folder}.`);
    // The folder's own question, not the one about the repos beside this one.
    assert.equal(out.questions[0]!.question, "Which repos should Carrick index?");
    assert.ok(!out.questions.some((entry) => entry.question.includes("same system")));
    assert.equal(fs.existsSync(path.join(fixture.app, ".carrick")), false);
    assert.ok(out.lines.includes("◇ Shop (shop): shop-api, shop-app"), out.lines.join("\n"));
  } finally {
    fixture.restore();
  }
  // A folder whose proposal does not name this repo is not its setup.
  const other = siblingFolder({});
  try {
    fs.mkdirSync(path.join(other.folder, ".carrick"), { recursive: true });
    fs.writeFileSync(path.join(other.folder, PROPOSAL_FILE), JSON.stringify({ repos: [{ path: other.api }] }));
    const out = siblingTerminal((rows) => [rows[0]!.value]);
    assert.equal(await initWith([other.app], out, true), 0, out.lines.join("\n"));
    assert.ok(out.questions[0]!.question.includes("same system"), out.questions[0]!.question);
  } finally {
    other.restore();
  }
});

// carrick#1512 review R4: a run that set up the folder above where it
// started says where to run init again, in every line that says to.
test("the lines that say to run init again name the folder when the run moved", posixNativeFixture, async () => {
  const fixture = siblingFolder({ connects: false, role: "member" });
  try {
    const out = siblingTerminal((rows) => rows.map((row) => row.value));
    out.step = async (_label, work, report) => {
      setImmediate(() => process.emit("SIGINT"));
      const value = await work(() => {});
      out.lines.push(report(value).text);
      return value;
    };
    assert.equal(await initWith([fixture.app], out, true), 0, out.lines.join("\n"));
    const said = out.lines.join("\n");
    assert.ok(out.lines.includes(` ${adminWait(`run carrick init in ${fixture.folder} again`)}`), said);
    assert.ok(
      said.includes(`then run carrick init --project shop in ${fixture.folder} again to verify.`),
      said,
    );
    assert.doesNotMatch(said, /run carrick init again|run init again/);
  } finally {
    fixture.restore();
  }
});

// carrick#1512 review: the folder's own carrick-workspace.json feeds the
// question, so a repo it already leaves out is not offered.
test("a repo the folder's workspace file leaves out is not offered beside this one", posixNativeFixture, async () => {
  const fixture = siblingFolder({ extra: 1 });
  try {
    fs.writeFileSync(path.join(fixture.folder, WORKSPACE_FILE), JSON.stringify({ exclude: ["shop-api"] }));
    const out = siblingTerminal((rows) => [rows[0]!.value]);
    assert.equal(await initWith([fixture.app], out, true), 0, out.lines.join("\n"));
    assert.deepEqual(
      out.questions[0]!.rows?.map((row) => row.label),
      ["shop-app (this repo)", "lib-00"],
    );
  } finally {
    fixture.restore();
  }
});

// carrick#1512 review: a repo with no GitHub remote goes into no project, so
// no project line names it — the question, the list, or the closing line.
test("a repo with no GitHub remote is named in no project line", posixNativeFixture, async () => {
  const fixture = siblingFolder({ unnamed: true });
  try {
    const out = siblingTerminal((rows) => rows.map((row) => row.value));
    assert.equal(await initWith([fixture.app], out, true), 0, out.lines.join("\n"));
    const said = out.lines.join("\n");
    assert.deepEqual(
      out.questions[0]!.rows?.find((row) => row.label === "notes"),
      { value: path.join(fixture.folder, "notes"), label: "notes", hint: "no GitHub remote" },
    );
    assert.ok(out.questions.some((entry) => entry.question === "Which project should shop-app and shop-api be in?"), said);
    assert.ok(said.includes("  Create project Shop with shop-app and shop-api\n"), said);
    assert.ok(out.lines.includes("◇ Shop (shop): shop-api, shop-app"), said);
  } finally {
    fixture.restore();
  }
});
