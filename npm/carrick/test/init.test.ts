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
import { deriveWorkspace, writeConfigs, type WorkspaceProposal } from "../src/init/repos.ts";
import {
  carrickHooks,
  hookCommand,
  mergeCarrickHooks,
  ownEntryPoint,
} from "../src/init/settings.ts";
import { editorLines, parseArgs, init } from "../src/init/run.ts";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

function executableInitFixture(
  projectSlug: string,
  // Whether this fixture's API has the project actions at all. "absent" is the
  // deployed server: it answers an action it has never heard of with the
  // credential-kind gate, which is a 403 (carrick#955).
  projectActions: "absent" | "deployed" = "absent",
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
  execFileSync("git", ["-C", repo, "remote", "add", "origin", "git@github.com:acme/api.git"]);

  const native = path.join(root, "native.mjs");
  fs.writeFileSync(native, `#!/usr/bin/env node
const argv = process.argv.slice(2);
if (argv[0] !== "derive") process.exit(2);
const workspace = argv[argv.indexOf("--workspace") + 1];
process.stdout.write(JSON.stringify({
  schema: "carrick.derive/0", workspace, repos_detected_by: "single repository",
  repos_added: [], repos_excluded: [], missing: [], parent_proposal: null,
  repos: [{ path: workspace, reason: "single repository", services: [{ serviceName: null }], config: null, warnings: [] }],
}));
`);
  fs.chmodSync(native, 0o755);

  const mockHttp = path.join(root, "mock-http.mjs");
  fs.writeFileSync(mockHttp, `
const created = new Set();
globalThis.fetch = async (input, init) => {
  if (String(input) !== "https://api.carrick.tools/types/check-or-upload") throw new Error("unexpected URL");
  const body = JSON.parse(String(init.body));
  if (body.action === "list-projects" || body.action === "create-project") {
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
    return Response.json({
      schema: "carrick.list-projects/0",
      projects: [
        { slug: "default", name: "Default", archived: false, repo_count: 1 },
        ...[...created].map((slug) => ({ slug, name: slug, archived: false, repo_count: 0 })),
      ],
    });
  }
  if (body.action !== "resolve-repos" || JSON.stringify(body.repos) !== JSON.stringify(["acme/api"])) throw new Error("unexpected request");
  return Response.json({
    schema: "carrick.resolve-repos/0",
    workspace: { slug: "acme", billing_tier: "free", installed: true },
    allowance_sentence: null,
    repos: [{ full_name: "acme/api", connected: true, project_id: "p1", project_slug: ${JSON.stringify(projectSlug)}, services: [] }],
    project_repos: [{ project_slug: ${JSON.stringify(projectSlug)}, repos: ["acme/api"] }],
  });
};
`);

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
    },
    cleanup: () => fs.rmSync(root, { recursive: true, force: true }),
  };
}

test("init writes a missing native proposal once and preserves racing or malformed files", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-init-"));
  try {
    const plan: WorkspaceProposal = { schema: "carrick.derive/0", workspace: dir, repos_detected_by: "single repository", repos_added: [], repos_excluded: [], missing: [], parent_proposal: null, repos: [{ path: dir, reason: "single repository", services: [{ serviceName: "api" }], config: { services: [{ name: "api", include: ["shared"] }] }, warnings: [] }] };
    const target = path.join(dir, "carrick.json");
    assert.equal(writeConfigs(plan)[0]?.created, true);
    const bytes = fs.readFileSync(target);
    assert.equal(writeConfigs(plan)[0]?.created, false);
    assert.deepEqual(fs.readFileSync(target), bytes);
    for (const body of ["{broken", '{"services":[{"name":"hand-added","include":["../shared"]}]}']) {
      fs.writeFileSync(target, body);
      assert.equal(writeConfigs(plan)[0]?.created, false);
      assert.equal(fs.readFileSync(target, "utf8"), body);
    }
    fs.unlinkSync(target);
    fs.mkdirSync(target);
    assert.equal(writeConfigs(plan)[0]?.created, false);
    assert.ok(fs.statSync(target).isDirectory());
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
    assert.equal(await init(["--yes", "--skip-index", dir]), 1);
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
  assert.deepEqual(parsed, { workspace: "/code", assumeYes: true, skipIndex: false, project: "payments" });
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

test("the executable CLI rejects a different project and makes no local setup claim", posixNativeFixture, () => {
  const fixture = executableInitFixture("default-project");
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", "--skip-index", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 1);
    assert.match(result.stdout, /acme\/api is currently in project "default-project"/);
    assert.match(result.stdout, /Create project "payments" if needed/);
    assert.match(result.stdout, /Assign the requested repos/);
    // The project actions are not deployed, so nothing was listed and the
    // browser still owns creating it.
    assert.doesNotMatch(result.stdout, /Projects in this workspace/);
    assert.doesNotMatch(result.stdout, /Created project/);
    assert.match(result.stderr, /Project "payments" was not verified/);
    assert.equal(fs.existsSync(path.join(fixture.repo, "carrick.json")), false);
    assert.equal(fs.existsSync(path.join(fixture.repo, ".claude")), false);
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
        [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", "--skip-index", fixture.repo],
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
// the list is printed, the project is created from here, and the browser is
// left with the one thing it still owns — assignment.
test("the executable CLI creates the named project when the API can", posixNativeFixture, () => {
  const fixture = executableInitFixture("default-project", "deployed");
  try {
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", "--skip-index", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 1);
    assert.match(result.stdout, /Projects in this workspace:/);
    assert.match(result.stdout, /^ {2}default {2}Default {2}1 repo$/m);
    assert.match(result.stdout, /Created project "payments"\./);
    assert.doesNotMatch(result.stdout, /Create project "payments" if needed/);
    assert.match(result.stdout, /Assign the requested repos/);
    assert.match(result.stderr, /Project "payments" was not verified/);
  } finally {
    fixture.cleanup();
  }
});

test("the executable CLI cannot verify a project without a GitHub repo identity", posixNativeFixture, () => {
  const fixture = executableInitFixture("payments");
  try {
    execFileSync("git", ["-C", fixture.repo, "remote", "remove", "origin"]);
    const result = spawnSync(
      process.execPath,
      [path.join(packageRoot, "bin", "carrick.mjs"), "init", "--project", "payments", "--yes", "--skip-index", fixture.repo],
      { cwd: fixture.repo, env: fixture.env, encoding: "utf8" },
    );
    assert.equal(result.status, 1);
    assert.match(result.stderr, /cannot verify .* because it has no GitHub origin/);
    assert.doesNotMatch(result.stdout, /Verified/);
  } finally {
    fixture.cleanup();
  }
});
