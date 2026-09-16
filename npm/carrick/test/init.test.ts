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
  mcpUnstampedLines,
  packagesLine,
  parseArgs,
  init,
  SCAFFOLD_SENTENCE,
} from "../src/init/run.ts";
import { hostedReport } from "../src/init/hosted.ts";
import { DOCS, interactiveOutput, plainOutput } from "../src/init/output.ts";
import { Writable } from "node:stream";
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
  workspace: {
    indexed?: boolean;
    alsoInProject?: string[];
    hostedState?: string;
    refreshFails?: boolean;
  } = {},
): {
  root: string;
  repo: string;
  env: NodeJS.ProcessEnv;
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
  fs.writeFileSync(native, `#!/usr/bin/env node
const argv = process.argv.slice(2);
const indexed = ${JSON.stringify(workspace.indexed === true)};
const at = (flag) => argv[argv.indexOf(flag) + 1];
if (indexed && argv[0] === "refresh") {
  if (${JSON.stringify(workspace.refreshFails === true)}) {
    process.stderr.write("carrick refresh: api has no carrick.json\\n");
    process.exit(1);
  }
  process.stdout.write("indexed 1 repo(s) in 4.0s\\n");
  process.exit(0);
}
if (indexed && argv[0] === "status") {
  const service = (name, state) => ({
    service: name, repo: at("--workspace"), index_commit: "abc1234",
    indexed_at: "2026-09-12T00:00:00Z", routes: 3, calls: 2, changed_since_index: 0,
    hosted_state: state,
  });
  const state = ${JSON.stringify(workspace.hostedState ?? "enriched")};
  process.stdout.write(JSON.stringify({
    schema: "carrick.status/0", workspace: at("--workspace"),
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
const created = new Set();
// The assignment this workspace currently holds, which \`assign-repos\` moves
// and \`resolve-repos\` then reads back: the CLI claims nothing it has not read.
let placed = ${JSON.stringify(projectSlug)};
globalThis.fetch = async (input, init) => {
  if (String(input) !== "https://api.carrick.tools/types/check-or-upload") throw new Error("unexpected URL");
  const body = JSON.parse(String(init.body));
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
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("unsigned init refuses GH_TOKEN before deriving, writing or requesting the network", async () => {
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

test("init reads its arguments", () => {
  const parsed = parseArgs(["-y", "--project", "payments", "--workspace", "/code"], "/tmp");
  assert.deepEqual(parsed, { workspace: "/code", assumeYes: true, project: "payments", repo: null });
  assert.equal((parseArgs(["--repo", "acme/api"]) as { repo: string }).repo, "acme/api");
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
  assert.match(parseArgs(["--help"]) as string, /--project SLUG/);
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
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /acme\/api is currently in project "default-project"/);
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
        '▲ Finish the browser steps above to put these repos in "payments", then run carrick init --project payments again to verify.',
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
      assert.ok(result.stdout.includes(SCAFFOLD_SENTENCE), result.stdout);
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
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Projects in this workspace:/);
    assert.match(result.stdout, /^ {2}default {2}Default {2}1 repo$/m);
    assert.match(result.stdout, /Created project "payments"\./);
    assert.doesNotMatch(result.stdout, /Create project "payments" if needed/);
    assert.match(result.stdout, /Moved acme\/api into project "payments"\./);
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
      result.stdout.includes("◇ Hosted index for 2 services downloaded into .carrick/"),
      result.stdout,
    );
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
    assert.ok(result.stdout.includes(SCAFFOLD_SENTENCE), result.stdout);
    assert.doesNotMatch(result.stdout, /is connected and has no hosted index yet/);
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
// found it apart from the ignored `.carrick` directory and the hook settings:
// carrick.json is the agent's to write, after someone has read it, and before
// the one paid scan (carrick-cloud#799). A change to this set is a deliberate
// diff in this list.
test("a first init writes the proposal, its ignore file and the hook settings, and nothing else", posixNativeFixture, () => {
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

    const onPath =
      spawnSync(process.platform === "win32" ? "where" : "which", ["carrick"], { stdio: "ignore" })
        .status === 0;
    assert.deepEqual(
      pathsUnder(fixture.repo),
      [
        path.join(".carrick", ".gitignore"),
        PROPOSAL_FILE,
        path.join(".claude", onPath ? "settings.json" : "settings.local.json"),
      ].sort(),
    );

    // The proposal is the whole derivation, including the config it would once
    // have written into the tree.
    // And the set is ignored where it has to be: all git can see in the tree
    // after a first run is the settings directory.
    assert.equal(
      execFileSync("git", ["-C", fixture.repo, "status", "--porcelain"], { encoding: "utf8" }),
      "?? .claude/\n",
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
        `  ${SCAFFOLD_SENTENCE}`,
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
    list: async () => null,
    create: async () => ({ kind: "absent" }),
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
  const prompts = recordingPrompts({
    list: async () => {
      throw new Error("a settled assignment is not a question to ask the API");
    },
  });
  assert.deepEqual(await projectStep("token", ["payments", "payments"], prompts), {
    slug: "payments",
    exists: true,
  });
  // The caller prints the assignment it then verifies, so this step adds no
  // second sentence about it.
  assert.deepEqual(prompts.lines, []);
  assert.deepEqual(prompts.asked, []);
});

test("a plain init asks nothing without a terminal, and settles nothing it would have to guess", async () => {
  for (const current of [["payments", "billing"], [null], []]) {
    const prompts = recordingPrompts();
    assert.deepEqual(await projectStep("token", current, prompts), { slug: null, exists: false });
    assert.deepEqual(prompts.asked, []);
  }
});

test("a plain init offers the list, and creates the project the terminal names", async () => {
  const created: string[] = [];
  const prompts = recordingPrompts({
    interactive: true,
    list: async () => LISTED,
    ask: async () => "search",
    confirm: async () => true,
    create: async (_token, slug) => {
      created.push(slug);
      return { kind: "created", project: { slug, name: slug, archived: false, repo_count: 0 } };
    },
  });
  assert.deepEqual(await projectStep("token", [null, "payments"], prompts), {
    slug: "search",
    exists: true,
  });
  assert.deepEqual(created, ["search"]);
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
    list: async () => LISTED,
    ask: async () => "default",
    create: async () => {
      throw new Error("an existing project is not one to create");
    },
  });
  assert.deepEqual(await projectStep("token", [null], prompts), { slug: "default", exists: true });
});

test("a plain init leaves the step to the browser on a refusal, an absent API or an unusable name", async () => {
  // An API without the actions is every workspace until the cloud half ships.
  const absent = recordingPrompts({ interactive: true, list: async () => null });
  assert.deepEqual(await projectStep("token", [null], absent), { slug: null, exists: false });

  const empty = recordingPrompts({ interactive: true, list: async () => LISTED, ask: async () => "" });
  assert.deepEqual(await projectStep("token", [null], empty), { slug: null, exists: false });

  const invalid = recordingPrompts({
    interactive: true,
    list: async () => LISTED,
    ask: async () => "Not A Slug",
  });
  assert.deepEqual(await projectStep("token", [null], invalid), { slug: null, exists: false });
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
      'Also in project "payments", not on this machine: acme/web.',
      'Also in project "search", not on this machine: acme/index.',
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
test("the plain rendering is one line per thing, with no colour and no box", () => {
  const written: string[] = [];
  const out = plainOutput((text) => void written.push(text));
  out.done("Repo acme/api connected");
  out.warn("Hosted index is older than this CLI");
  out.refuse("Hosted index could not be read into .carrick/: no reason");
  out.say("Connect repositories in your browser: https://app.carrick.tools/repos");
  out.note("Next: paste this to your agent", [SCAFFOLD_SENTENCE]);
  assert.deepEqual(written.join("").split("\n"), [
    "◇ Repo acme/api connected",
    "▲ Hosted index is older than this CLI",
    "■ Hosted index could not be read into .carrick/: no reason",
    "Connect repositories in your browser: https://app.carrick.tools/repos",
    "",
    "Next: paste this to your agent",
    `  ${SCAFFOLD_SENTENCE}`,
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
    hostedReport({ kind: "local_only", services: 3, state: "read_failed" }),
  );
  await out.step("Reading the hosted index into .carrick/", async () => "downloaded", () =>
    hostedReport({ kind: "downloaded", services: 2 }),
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
    "◇ Hosted index for 2 services downloaded into .carrick/",
  ]);
  // The label is the spinner's while it spins, and nothing once it stops.
  assert.doesNotMatch(plain.split("\n").filter((line) => /[◇▲■]/.test(line)).join("\n"), /Reading the hosted index/);
});

// The four index states, as lines rather than as tags. Each one says what this
// machine now holds; the two with something to do about it carry the marker
// that says so, and the one that produced nothing is a refusal (carrick#1020,
// carrick#1026).
test("every hosted outcome is one line, and only an actionable one is a warning", () => {
  assert.deepEqual(hostedReport({ kind: "downloaded", services: 2 }), {
    kind: "done",
    text: "Hosted index for 2 services downloaded into .carrick/",
  });
  assert.deepEqual(hostedReport({ kind: "downloaded", services: 1 }).text, "Hosted index for 1 service downloaded into .carrick/");
  const older = hostedReport({ kind: "version_mismatch", services: 2 });
  assert.equal(older.kind, "warn");
  assert.match(older.text, /run `carrick index --detach` once from main/);
  // And never a downgrade: `CACHE_VERSION` moves most weeks (carrick#1012).
  assert.doesNotMatch(older.text, /npm i -g carrick@/);
  const local = hostedReport({ kind: "local_only", services: 3, state: "commit_missing" });
  assert.equal(local.kind, "warn");
  assert.match(local.text, /\.carrick\/ holds 3 services as this machine read them; the commit/);
  // The state this run can cause itself: a repo it just connected under a name
  // the scanner cannot read back off the git remote (carrick#1056). The clause
  // has to name the remote as the reason, not the connection.
  const unnamed = hostedReport({ kind: "local_only", services: 1, state: "remote_unnamed" });
  assert.match(unnamed.text, /git remote here names no owner\/repo/);
  assert.doesNotMatch(unnamed.text, /not connected/);
  const failed = hostedReport({ kind: "failed", problem: "api has no carrick.json" });
  assert.deepEqual(failed, {
    kind: "refuse",
    text: "Hosted index could not be read into .carrick/: api has no carrick.json",
  });
  // No branch forbids a command: the sentence that did left the reader with no
  // index and nothing that would build one (carrick#1020).
  for (const outcome of [older, local, failed]) {
    assert.doesNotMatch(outcome.text, /Do not run/);
  }
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

// A client connected before the install id existed. `carrick init` does not
// take somebody's Claude Code entry out and write it again — that would lose
// whatever else is on it and send the next session back through the server's
// OAuth — so it states the pair that does it and changes nothing
// (carrick-cloud#890).
test("an entry with no install id is one warning, and the commands that fix it", () => {
  const add =
    'claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp ' +
    '--header "X-Carrick-Install-Id: 11111111-2222-4333-8444-555555555555"';
  const mcp = [
    {
      client: "Claude Code",
      state: "unstamped",
      detail: `MCP entry has no install id. To add it: claude mcp remove --scope user carrick && ${add}`,
    } as const,
    { client: "Cursor", state: "written", detail: "/home/.cursor/mcp.json" } as const,
  ];
  assert.deepEqual(mcpUnstampedLines(mcp), [
    "Claude Code: MCP entry has no install id. To add it: " +
      `claude mcp remove --scope user carrick && ${add}`,
  ]);
  // Nothing was configured for Claude Code, so the setup line does not say it
  // was, and the client is not counted among the files this run wrote.
  assert.equal(configuredLine(mcp), "Claude Code hooks configured");
  assert.deepEqual(mcpClientLines(mcp), ["MCP added for Cursor: /home/.cursor/mcp.json"]);
});

test("the connected repos are one line however many there are", () => {
  assert.equal(connectedLine(["acme/api"]), "Repo acme/api connected");
  assert.equal(connectedLine(["acme/api", "acme/web"]), "2 repos connected: acme/api, acme/web");
  assert.equal(
    connectedLine(["a/1", "a/2", "a/3", "a/4", "a/5"]),
    "5 repos connected: a/1, a/2, a/3 and 2 more",
  );
});
