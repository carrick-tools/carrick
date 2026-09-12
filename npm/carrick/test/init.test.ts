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
import { absentRepos, agentScaffoldPrompt, editorLines, parseArgs, init } from "../src/init/run.ts";
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
  workspace: { indexed?: boolean; alsoInProject?: string[] } = {},
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
  // Anything but `derive` exits 2, so a run that ends 0 is a run that never
  // asked this binary to scan.
  fs.writeFileSync(native, `#!/usr/bin/env node
const argv = process.argv.slice(2);
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
      CARRICK_NATIVE_BINARY: native,
      CARRICK_TOKEN: "test-token",
      XDG_CONFIG_HOME: path.join(root, "config"),
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

// The gallery an id resolves against is that editor's own, and the three
// editors named below read three different ones. The extension is published to
// each of those three, so an id is only ever printed for an editor whose
// gallery carries it (carrick#915).
test("no editor is handed an id for a gallery it does not read", () => {
  const everyEditor = editorLines(() => true);
  for (const line of everyEditor) {
    const install = /^\s*(\S+) --install-extension (\S+)/.exec(line);
    if (!install) continue;
    const [, editor, target] = install;
    assert.ok(
      ["code", "cursor", "windsurf"].includes(editor!),
      `${editor} resolves an id against a gallery this extension is not published to: ${line}`,
    );
    assert.equal(target, "carrick-tools.carrick");
  }
  // Every editor answers in a heading and a command a user can copy, so a block
  // that has grown prose is a block that is explaining something away.
  assert.equal(everyEditor.length, 6);
});

test("VS Code gets the gallery command like the others", () => {
  const code = editorLines((command) => command === "code");
  assert.deepEqual(code, [
    "  VS Code, for diagnostics in the Problems panel:",
    "    code --install-extension carrick-tools.carrick",
  ]);
  // A pointer somewhere else is what a missing publish reads like, and there is
  // no missing publish.
  assert.doesNotMatch(code.join("\n"), /\.vsix|docs\.carrick\.tools|Marketplace/);
});

test("Cursor and Windsurf get the gallery command, which is one they can run", () => {
  const cursor = editorLines((command) => command === "cursor");
  assert.deepEqual(cursor, [
    "  Cursor, for diagnostics in the Problems panel:",
    "    cursor --install-extension carrick-tools.carrick",
  ]);
  assert.match(
    editorLines((command) => command === "windsurf").join("\n"),
    /windsurf --install-extension carrick-tools\.carrick/,
  );

  // Both editors on one machine is two blocks, and an editor that is not on the
  // machine is not named.
  const forks = editorLines((command) => command === "cursor" || command === "windsurf");
  assert.equal(forks.length, 4);
  assert.doesNotMatch(forks.join("\n"), /VS Code|code --install-extension/);
});

test("an editor we have not tested gets the server's command and no claim", () => {
  const unknown = editorLines(() => false);
  assert.equal(unknown.length, 1);
  assert.match(unknown[0]!, /carrick lsp --stdio/);
  assert.doesNotMatch(unknown[0]!, /--install-extension/);
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
  const previous = process.env["CARRICK_NATIVE_BINARY"];
  try {
    const native = path.join(dir, "native.mjs");
    const said = "carrick derive: no repos in /code.\\nLooked for: carrick.json, ...\\nFound: none of those manifests in /code. Inside it: no directories.";
    fs.writeFileSync(native, `#!/usr/bin/env node\nprocess.stderr.write("${said}\\n");\nprocess.exit(1);\n`);
    fs.chmodSync(native, 0o755);
    process.env["CARRICK_NATIVE_BINARY"] = native;
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
    if (previous === undefined) delete process.env["CARRICK_NATIVE_BINARY"];
    else process.env["CARRICK_NATIVE_BINARY"] = previous;
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
        'Setup continues; finish the browser steps above to put these repos in "payments", then run carrick init --project payments again to verify.',
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
      assert.match(result.stdout, /Verified 1 repo in project "payments"/);
      assert.doesNotMatch(result.stdout, /Create project "payments" if needed/);
      // A repo already in the project is not a project to look up or create.
      assert.doesNotMatch(result.stdout, /Projects in this workspace/);
      // Setup ends where the dashboard's checklist ends: the prompt that makes
      // an agent write this repo's workflow and carrick.json (carrick#955).
      assert.match(result.stdout, /Run the carrick scaffold tool/);
      assert.match(result.stdout, /carrick\.json/);
      // No agent client under this fixture's home, so the MCP step states the
      // line rather than claiming a connection.
      assert.match(result.stdout, /claude mcp add --scope user --transport http carrick/);
      assert.match(result.stdout, /No agent client was found on this machine/);
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
    assert.match(result.stdout, /Verified 1 repo in project "payments"/);
    // And no browser step is asked for, because none is left.
    assert.doesNotMatch(result.stdout, /Assign the requested repos/);
    assert.doesNotMatch(result.stdout, /Setup continues/);
  } finally {
    fixture.cleanup();
  }
});

// carrick#993 rows 2 and 18: what a second machine joining an indexed project
// is told. The paid scan has already run in CI, and the project holds repos
// this folder does not.
test("the executable CLI refuses the paid scan on an indexed repo and names the rest of the project", posixNativeFixture, () => {
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
    assert.ok(
      result.stdout.includes("This repo already has a hosted index. Run `carrick index`"),
      result.stdout,
    );
    assert.match(result.stdout, /a laptop scan from a branch replaces the/);
    assert.doesNotMatch(result.stdout, /There is no index yet/);
    // Including in the prompt the run ends on, which is the copy an agent acts
    // on rather than reads.
    assert.match(result.stdout, /Do not run `carrick index --infer`/);
    assert.doesNotMatch(result.stdout, /is connected and has no hosted index yet/);
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

    // And the run ends on the prompt that turns it into a config. This
    // workspace read reports no services, so it is the prompt that runs the
    // one paid scan.
    assert.match(result.stdout, /Paste this to your agent:/);
    assert.equal(result.stdout.trimEnd().endsWith(agentScaffoldPrompt(false)), true, result.stdout.slice(-400));
    assert.match(result.stdout, /There is no index yet\./);
  } finally {
    fixture.cleanup();
  }
});

// carrick#960: this prompt is a copy of the scaffold tool's own instructions,
// in another repository, and it once named a file the tool had stopped
// returning. A drifted copy fails here rather than in a user's terminal.
test("the scaffold prompt names only files that seam owns, and states the sequence", () => {
  for (const prompt of [agentScaffoldPrompt(false), agentScaffoldPrompt(true)]) {
    const named: string[] = prompt.match(/[\w./-]*\.(?:json|ya?ml|md)/g) ?? [];
    for (const file of named) {
      assert.ok(
        [".carrick/proposal.json", "carrick.json", "AGENTS.md", ".github/workflows/carrick.yml"].includes(file),
        `${file} is not a file the scaffold tool writes or reads`,
      );
    }
    assert.ok(named.includes(".carrick/proposal.json"), prompt);
    assert.ok(named.includes("carrick.json"), prompt);
    // The free pass is in both: it is what proves the config, and it costs
    // nothing to run against an index CI already built.
    assert.match(prompt, /`carrick index`, which is free/);
  }
  // Free pass first, one paid scan after it (carrick-cloud#799).
  const fresh = agentScaffoldPrompt(false);
  assert.match(fresh, /`carrick index --infer` once/);
  assert.ok(fresh.indexOf("`carrick index`") < fresh.indexOf("--infer"));
  // And where CI has already built the index, the paid scan is refused rather
  // than ordered: a laptop scan from a branch replaces that row for the whole
  // workspace (carrick#993 row 2).
  const hosted = agentScaffoldPrompt(true);
  assert.match(hosted, /Do not run `carrick index --infer`/);
  assert.doesNotMatch(hosted, /Then run `carrick index --infer` once/);
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
    assert.match(result.stdout, /Repos requested for project "payments":\n {2}acme\/api/);
    assert.match(result.stdout, /Verified 1 repo in project "payments"/);
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
    assert.ok(result.stdout.includes(`Taking acme/api as the GitHub repository for ${fixture.repo}`), result.stdout);
    assert.match(result.stdout, /Verified 1 repo in project "payments"/);
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
