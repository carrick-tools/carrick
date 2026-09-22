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
import { PROJECT_RULE, projectStep, type Project, type ProjectPrompts } from "../src/init/projects.ts";
import {
  absentRepos,
  configuredLine,
  connectedLine,
  mcpClientLines,
  packagesLine,
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
  editorClause,
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
  repos: [{ path: workspace, reason: "single repository", services: [{ serviceName: null }], config: null, warnings: [] }],
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
  let binDirectory: string | null = null;
  if (clone.sshHostname !== undefined) {
    binDirectory = path.join(root, "bin");
    fs.mkdirSync(binDirectory);
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
      ...(binDirectory === null
        ? {}
        : { PATH: `${binDirectory}${path.delimiter}${process.env["PATH"] ?? ""}` }),
    },
    cleanup: () => fs.rmSync(root, { recursive: true, force: true }),
  };
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
    const plan: WorkspaceProposal = { schema: "carrick.derive/0", workspace: dir, repos_detected_by: "single repository", repos_added: [], repos_excluded: [], missing: [], parent_proposal: null, repos: [{ path: dir, reason: "single repository", services: [{ serviceName: "api" }], config: { services: [{ name: "api", include: ["shared"] }] }, warnings: [] }] };
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
  const previous = { ...process.env };
  const fetchBefore = globalThis.fetch;
  try {
    process.env["XDG_CONFIG_HOME"] = dir;
    process.env["GH_TOKEN"] = "github-is-not-carrick";
    delete process.env["CARRICK_TOKEN"];
    globalThis.fetch = async () => { throw new Error("must not request network"); };
    assert.equal(await init(["--yes", dir]), 1);
    assert.deepEqual(fs.readdirSync(dir), []);
  } finally {
    process.env = previous;
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
    assert.match(result.stdout, /acme\/api is currently in project default-project/);
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
    // of — display name and slug, which is what the dashboard shows it under.
    assert.match(
      result.stdout,
      /acme\/api will move from Acme Default \(default-project\) to payments/,
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
    assert.match(result.stderr, /acme\/api is in project Acme Default \(default-project\)/);
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
    { path: ${JSON.stringify(made["api"])}, reason: "single repository", services: [{ serviceName: "api" }], config: null, warnings: [] },
    { path: ${JSON.stringify(made["web"])}, reason: "npm workspaces", services: [{ serviceName: "web" }, { serviceName: "admin" }], config: null, warnings: [] },
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
    // And recorded as ours, which is what `carrick remove` takes back.
    assert.deepEqual(selection.carrick, { exclude: ["web"] });
    assert.match(result.stdout, /web excluded in carrick-workspace\.json/);
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
      // The project is stated once, where the login is (carrick#1026), and the
      // line that used to repeat it as a verdict is gone.
      assert.ok(result.stdout.includes("◇ Signed in as acme · project payments"), result.stdout);
      assert.doesNotMatch(result.stdout, /Verified 1 repo in project/);
      assert.doesNotMatch(result.stdout, /Create project "payments" if needed/);
      // A repo already in the project is not a project to look up or create.
      assert.doesNotMatch(result.stdout, /Projects in this workspace/);
      // Setup ends where the dashboard's checklist ends: one sentence naming
      // the scaffold tool, which carries the instructions (cloud#832).
      assert.ok(result.stdout.includes(scaffoldFor("acme/api")), result.stdout);
      // No agent client under this fixture's home, so the MCP step states the
      // line rather than claiming a connection. The line carries this
      // machine's install id, which the run has just minted
      // (carrick-cloud#890).
      const installId = fs.readFileSync(path.join(fixture.root, "home", ".carrick", "install-id"), "utf8").trim();
      assert.match(installId, /^[A-Za-z0-9_-]{8,64}$/);
      assert.ok(
        result.stdout.includes(
          "▲ No agent client found on this machine. In Claude Code: claude mcp add --scope user " +
            `--transport http carrick https://api.carrick.tools/mcp --header "X-Carrick-Install-Id: ${installId}"`,
        ),
        result.stdout,
      );
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
    assert.match(result.stdout, /Created project "payments"\./);
    assert.doesNotMatch(result.stdout, /Create project "payments" if needed/);
    assert.match(result.stdout, /Moved acme\/api into project payments\./);
    // The move is named in the proposal, with the project it comes out of, and
    // it is named BEFORE the request that performs it.
    const proposed = result.stdout.indexOf("acme/api will move from Acme Default (default-project) to payments");
    assert.ok(proposed >= 0, result.stdout);
    assert.ok(proposed < result.stdout.indexOf("Moved acme/api"), result.stdout);
    // Claimed only because resolve-repos read it back afterwards.
    assert.ok(result.stdout.includes("◇ Signed in as acme · project payments"), result.stdout);
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
    assert.ok(result.stdout.trimEnd().endsWith(`Docs: ${DOCS}`), result.stdout);
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
    assert.ok(result.stdout.trimEnd().endsWith(`Docs: ${DOCS}`), result.stdout);
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

    // The run says it installed them, so the wiring is reached and not only
    // the writer underneath it.
    assert.match(result.stdout, /Task skills installed in .+: carrick-impact, /);

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

    // And the run ends where the ticket rules it ends: the state of the index,
    // one sentence for the agent, and the link that carries everything else
    // (carrick#1026). This workspace read reports no services, so the scan is
    // still to run.
    assert.equal(
      result.stdout.trimEnd().split("\n").slice(-6).join("\n"),
      [
        "◇ No index yet: your agent runs the one scan",
        "",
        "Next: paste this to your agent",
        `  ${scaffoldFor("acme/api")}`,
        "",
      ].join("\n") + `\nDocs: ${DOCS}`,
      result.stdout,
    );
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
    say: (line: string) => void lines.push(line),
    ask: async (question: string) => {
      asked.push(question);
      return "";
    },
    confirm: async (question: string) => {
      asked.push(question);
      return true;
    },
    interactive: false,
    assumeYes: false,
    ...overrides,
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

test("a plain init offers the list, and creates the project the terminal names", async () => {
  const prompts = recordingPrompts({
    interactive: true,
    ask: async () => "search",
    confirm: async () => true,
  });
  // Named, and asked for: the creation itself waits for the proposal, so what
  // this step answers is what the run has permission to do (carrick#1338).
  assert.deepEqual(await projectStep([null, "payments"], LISTED, prompts), {
    slug: "search",
    exists: false,
    create: true,
  });
  assert.ok(prompts.lines.some((line) => line.includes("Projects in this workspace:")));
  assert.ok(prompts.lines.some((line) => line.includes("payments")));
  // What a project is, said above the question rather than in the docs: a
  // project is the boundary every cross-repo answer is computed inside, and
  // splitting one system across two of them is the mistake this prevents
  // (carrick#993 row 13).
  const rule = prompts.lines.indexOf(PROJECT_RULE);
  assert.ok(rule >= 0, prompts.lines.join("\n"));
  assert.ok(rule < prompts.lines.findIndex((line) => line.includes("Projects in this workspace:")));
});

test("a plain init takes a listed project without creating anything", async () => {
  const prompts = recordingPrompts({
    interactive: true,
    ask: async () => "default",
    confirm: async () => {
      throw new Error("an existing project is not one to offer to create");
    },
  });
  assert.deepEqual(await projectStep([null], LISTED, prompts), {
    slug: "default",
    exists: true,
    create: false,
  });
});

test("a plain init leaves the step to the browser on a refusal, an absent API or an unusable name", async () => {
  const none = { slug: null, exists: false, create: false };
  // An API without the actions is every workspace until the cloud half ships.
  const absent = recordingPrompts({ interactive: true });
  assert.deepEqual(await projectStep([null], null, absent), none);

  const empty = recordingPrompts({ interactive: true, ask: async () => "" });
  assert.deepEqual(await projectStep([null], LISTED, empty), none);

  const invalid = recordingPrompts({
    interactive: true,
    ask: async () => "Not A Slug",
  });
  assert.deepEqual(await projectStep([null], LISTED, invalid), none);
  assert.ok(invalid.lines.some((line) => line.includes("is not a project slug")));
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
    assert.ok(result.stdout.includes("◇ Repo acme/api connected"), result.stdout);
    assert.ok(result.stdout.includes("◇ Signed in as acme · project payments"), result.stdout);
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
    assert.ok(result.stdout.includes("◇ Signed in as acme · project payments"), result.stdout);
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
    () => interactive.ask("Which project should these repos be in?"),
    () =>
      interactive.choose(
        "Which repos does this install cover?",
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
  repos: [{ path: workspace, reason: "single repository", services: [{ serviceName: "api" }], config: null, warnings: [] }],
}));
`);
  fs.chmodSync(native, 0o755);
  const previous = { ...process.env };
  const fetchBefore = globalThis.fetch;
  const asked: string[] = [];
  process.env["CARRICK_BIN"] = native;
  process.env["CARRICK_TOKEN"] = "test-token";
  process.env["XDG_CONFIG_HOME"] = path.join(root, "config");
  process.env["HOME"] = path.join(root, "home");
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
      process.env = previous;
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
    const out = recordingOutput({ confirm: async () => false });
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
    assert.deepEqual(questions, ["Move acme/api out of Acme Default (default-project) into Payments (payments)?"]);
    assert.deepEqual(fs.readdirSync(fixture.repo), [".git"]);
    assert.deepEqual(fixture.asked, ["resolve-repos", "list-projects"]);
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
  const scan = (state: RunningScan["status"]): RunningScan => ({
    scan_id: "s1",
    pid: 4242,
    status: state,
    started_at: "2026-09-22T00:00:00Z",
  });
  assert.deepEqual(
    localIndexState(statusAnswer({ services: [], running_scans: [scan("running")] }), ["/code/api"]),
    { kind: "scanning" },
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

test("the derived line names the manifest kind only where every repo agrees", () => {
  const plan = (repos: Array<{ reason: string; services: number }>): WorkspaceProposal => ({
    schema: "carrick.derive/0",
    workspace: "/code",
    repos_detected_by: "test",
    repos_added: [],
    repos_excluded: [],
    missing: [],
    parent_proposal: null,
    repos: repos.map((repo) => ({
      path: "/code",
      reason: repo.reason,
      services: Array.from({ length: repo.services }, (_, index) => ({ serviceName: `s${index}` })),
      config: null,
      warnings: [],
    })),
  });
  assert.equal(packagesLine(plan([{ reason: "Deno manifests", services: 4 }])), `4 Deno packages found, proposal in ${PROPOSAL_FILE}`);
  assert.equal(packagesLine(plan([{ reason: "npm workspaces", services: 1 }])), `1 npm package found, proposal in ${PROPOSAL_FILE}`);
  assert.equal(packagesLine(plan([{ reason: "pnpm workspaces", services: 9 }])), `9 pnpm packages found, proposal in ${PROPOSAL_FILE}`);
  // A repo with no workspace manifests, and a workspace of two kinds: neither
  // has one word for what was found, so neither gets one.
  assert.equal(packagesLine(plan([{ reason: "single repository", services: 1 }])), `1 package found, proposal in ${PROPOSAL_FILE}`);
  assert.equal(
    packagesLine(plan([{ reason: "npm workspaces", services: 2 }, { reason: "Deno manifests", services: 3 }])),
    `5 packages found, proposal in ${PROPOSAL_FILE}`,
  );
});

// A client is named only where this run changed something for it: a first run
// used to print four lines about clients it had left exactly as they were
// (carrick#1026).
test("the setup line names the clients this run changed, and no others", () => {
  assert.equal(
    configuredLine([{ client: "Claude Code", state: "written", detail: "connected for this user" }]),
    "Claude Code hooks and MCP configured (restart the client)",
  );
  assert.equal(
    configuredLine([{ client: "Claude Code", state: "present", detail: 'already connected as "carrick"' }]),
    "Claude Code hooks configured",
  );
  // Another client is its own line, naming the file this run guessed at and
  // wrote: a wrong guess has to be one line and one entry to delete.
  const machine = [
    { client: "Claude Code", state: "written", detail: "connected for this user" } as const,
    { client: "Cursor", state: "written", detail: "/home/.cursor/mcp.json" } as const,
    { client: "Windsurf", state: "present", detail: "already in /home/.codeium/windsurf/mcp_config.json" } as const,
    { client: "VS Code", state: "failed", detail: "not valid JSON" } as const,
  ];
  assert.equal(configuredLine(machine), "Claude Code hooks and MCP configured (restart the client)");
  assert.deepEqual(mcpClientLines(machine), ["MCP added for Cursor: /home/.cursor/mcp.json"]);
});

// An entry with no install id is not news. The id is a field on the server's
// own log line; adding one to a working entry costs the sign-in, because
// Claude Code keys its stored OAuth record on the headers (carrick#1365). The
// run says nothing about it, which is what leaves the warning count at what
// the reader can act on.
test("an entry with no install id produces no line at all", () => {
  const mcp = [
    { client: "Claude Code", state: "present", detail: 'already connected as "carrick"' } as const,
    { client: "Cursor", state: "written", detail: "/home/.cursor/mcp.json" } as const,
  ];
  assert.equal(configuredLine(mcp), "Claude Code hooks configured");
  assert.deepEqual(mcpClientLines(mcp), ["MCP added for Cursor: /home/.cursor/mcp.json"]);
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

  assert.equal(editorClause(offered, ["Cursor"]), ", and add Carrick to /home/dev/.cursor/mcp.json");
  assert.equal(editorClause(offered, []), "");
});

test("the connected repos are one line however many there are", () => {
  assert.equal(connectedLine(["acme/api"]), "Repo acme/api connected");
  assert.equal(connectedLine(["acme/api", "acme/web"]), "2 repos connected: acme/api, acme/web");
  assert.equal(
    connectedLine(["a/1", "a/2", "a/3", "a/4", "a/5"]),
    "5 repos connected: a/1, a/2, a/3 and 2 more",
  );
});
