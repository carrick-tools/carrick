// What `carrick remove` takes off a machine, and what it refuses to touch.
//
// The two that matter are the two that can destroy something: a settings file
// holding somebody else's hooks, and a repository holding committed files.
// The first is edited and tested for what it leaves; the second is only ever
// listed. Every test here states its own HOME, XDG_CONFIG_HOME and workspace,
// because the subject is a user's own machine and a test that reached this one
// would be dismantling it.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { mergeCarrickHooks, removeCarrickHooks } from "../src/init/settings.ts";
import {
  parseArgs,
  repoLeftovers,
  mcpRemovalLines,
  SCAFFOLD_FILES,
  SETTINGS_FILES,
} from "../src/init/remove.ts";
import { MCP_URL } from "../src/init/mcp.ts";
import { INSTALL_ID_HEADER, installIdPath } from "../src/init/install-id.ts";
import { sessionFile, sessionsDir } from "../src/hook/reuse.ts";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
/** What this machine calls itself to the index, until this command deletes it. */
const INSTALL_ID = "11111111-2222-4333-8444-555555555555";

test("remove reads its arguments", () => {
  assert.deepEqual(parseArgs([], "/work"), { workspace: "/work", keepLogin: false });
  assert.deepEqual(parseArgs(["--keep-login"], "/work"), { workspace: "/work", keepLogin: true });
  assert.deepEqual(parseArgs(["repo"], "/work"), { workspace: "/work/repo", keepLogin: false });
  assert.deepEqual(parseArgs(["-w", "/elsewhere"], "/work"), { workspace: "/elsewhere", keepLogin: false });
  assert.match(parseArgs(["--workspace"], "/work") as string, /needs a directory/);
  assert.match(parseArgs(["--nonsense"], "/work") as string, /unknown option/);
  assert.match(parseArgs(["--help"], "/work") as string, /^carrick remove/);
});

test("the hooks the writer merged in are exactly the hooks the remover takes out", () => {
  // A file holding someone else's hooks, the hosted index's hook pack, and a
  // key of its own: the round trip has to give all three back untouched.
  const before = JSON.stringify(
    {
      permissions: { allow: ["Bash(ls:*)"] },
      hooks: {
        SessionStart: [
          { hooks: [{ type: "command", command: '"$CLAUDE_PROJECT_DIR/.claude/session-start.sh"' }] },
        ],
        PostToolUse: [
          { matcher: "Write|Edit", hooks: [{ type: "command", command: "eslint --fix" }] },
        ],
      },
    },
    null,
    2,
  );
  for (const command of ["carrick", '"/opt/my tools/carrick/bin/carrick.mjs"']) {
    const written = mergeCarrickHooks(before, command);
    assert.equal(written.changed, true);
    const removed = removeCarrickHooks(written.body);
    assert.equal(removed.changed, true);
    assert.deepEqual(JSON.parse(removed.body), JSON.parse(before));
  }

  // And a second run has nothing left to change, which is what the command
  // reports as "nothing to remove".
  const cleaned = removeCarrickHooks(removeCarrickHooks(mergeCarrickHooks(before).body).body);
  assert.equal(cleaned.changed, false);
});

test("a settings file holding no entry of ours is not touched, and not claimed", () => {
  // Neither of these is in the format the merge writes, and one has no hooks
  // key at all: reformatting somebody else's committed file — and printing a
  // line saying something was removed from it — is the failure this guards.
  const documents = [
    '{\n    "permissions": { "allow": ["Bash(ls:*)"] }\n}',
    '{\n    "hooks": {\n        "PostToolUse": [\n            { "hooks": [{ "type": "command", "command": "eslint --fix" }] }\n        ]\n    }\n}',
    '{\n  "hooks": {\n    "PreToolUse": [\n      { "matcher": "Grep", "hooks": [{ "type": "command", "command": "carrickctl hook post-edit" }] }\n    ]\n  }\n}\n',
    "{}",
  ];
  for (const document of documents) {
    const outcome = removeCarrickHooks(document);
    assert.equal(outcome.changed, false, document);
    assert.equal(outcome.body, document, document);
  }

  // A file that is not JSON is still reported rather than replaced.
  assert.throws(() => removeCarrickHooks("{ this is not json"));
});

test("the repo files are listed where they are, and the merged sections are named", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-remove-scan-"));
  try {
    const write = (relative: string, body: string): void => {
      const target = path.join(root, relative);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, body);
    };
    for (const relative of SCAFFOLD_FILES) write(relative, "x");
    write("AGENTS.md", "# Repo\n\n## Carrick\n\nAsk Carrick first.\n");
    write(".gitignore", ".claude/*\n!.claude/settings.json\n");
    write(".claude/settings.json", JSON.stringify({ hooks: { SessionStart: [{ hooks: [{ type: "command", command: '"$CLAUDE_PROJECT_DIR/.claude/session-start.sh"' }] }] } }));

    const leftovers = repoLeftovers(root);
    assert.deepEqual(leftovers.files, SCAFFOLD_FILES);
    assert.deepEqual(leftovers.sections, [
      'AGENTS.md: the "## Carrick" section',
      ".claude/settings.json: the hook-pack entries the scaffold merged in",
      ".gitignore: the .claude negations",
    ]);

    // A folder of repos: only a child that is a repository is looked into, and
    // the paths stay relative to the workspace so the git rm line is one line.
    const parent = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-remove-parent-"));
    try {
      const repo = path.join(parent, "api");
      fs.mkdirSync(path.join(repo, ".github", "workflows"), { recursive: true });
      fs.writeFileSync(path.join(repo, ".git"), "gitdir: elsewhere\n");
      fs.writeFileSync(path.join(repo, ".github", "workflows", "carrick.yml"), "x");
      fs.mkdirSync(path.join(parent, "notes"), { recursive: true });
      fs.writeFileSync(path.join(parent, "notes", "carrick.json"), "x");
      assert.deepEqual(repoLeftovers(parent).files, [path.join("api", ".github", "workflows", "carrick.yml")]);
    } finally {
      fs.rmSync(parent, { recursive: true, force: true });
    }

    // Nothing scaffolded: nothing to say.
    const empty = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-remove-empty-"));
    try {
      assert.deepEqual(repoLeftovers(empty), { files: [], sections: [] });
    } finally {
      fs.rmSync(empty, { recursive: true, force: true });
    }
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("a client left alone is a warning, and a client changed is a done line", () => {
  const lines = mcpRemovalLines([
    { client: "Claude Code", state: "removed", detail: "MCP server removed for this user" },
    { client: "Cursor", state: "removed", detail: "/home/dev/.cursor/mcp.json" },
    { client: "Windsurf", state: "kept", detail: '"carrick" there does not point at api.carrick.tools, so it was left alone' },
    { client: "VS Code", state: "absent", detail: "no carrick server" },
    { client: "Codex", state: "failed", detail: "remove it by hand" },
  ]);
  assert.deepEqual(lines.done, [
    "MCP server removed for Claude Code",
    "MCP server removed for Cursor: /home/dev/.cursor/mcp.json",
  ]);
  assert.deepEqual(lines.warn, [
    'Windsurf: "carrick" there does not point at api.carrick.tools, so it was left alone',
    "MCP server not removed for Codex: remove it by hand",
  ]);
});

/**
 * A machine with everything `carrick init` writes on it: an agent client, a
 * credential, an install id, hook entries beside somebody else's, and a
 * scaffolded repo.
 *
 * `claude` is a fake on PATH that logs what it was asked and answers `mcp get`
 * with the URL the real one prints. Without it the run would reach the real
 * client, which is this developer's own configuration.
 */
function machine(): { root: string; home: string; workspace: string; env: NodeJS.ProcessEnv; log: string } {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-remove-cli-"));
  const home = path.join(root, "home");
  const workspace = path.join(root, "repo");
  const bin = path.join(root, "bin");
  const log = path.join(root, "claude.log");
  fs.mkdirSync(path.join(home, ".claude"), { recursive: true });
  fs.mkdirSync(path.join(home, ".cursor"), { recursive: true });
  fs.mkdirSync(bin, { recursive: true });

  const claude = path.join(bin, "claude");
  // The fake keeps the one piece of state the real client keeps: a server it
  // has been asked to remove is gone from the next `mcp get`. Without it the
  // second run would report removing the same entry again, and the idempotency
  // this test is about would be the fixture's, not the command's.
  const removed = path.join(root, "claude-removed");
  fs.writeFileSync(claude, `#!/usr/bin/env node
const fs = require("node:fs");
const argv = process.argv.slice(2);
fs.appendFileSync(${JSON.stringify(log)}, argv.join(" ") + "\\n");
if (argv[1] === "get") {
  if (fs.existsSync(${JSON.stringify(removed)})) process.exit(1);
  process.stdout.write("carrick:\\n  Scope: User config\\n  Type: http\\n  URL: ${MCP_URL}\\n");
}
if (argv[1] === "remove") fs.writeFileSync(${JSON.stringify(removed)}, "");
process.exit(0);
`);
  fs.chmodSync(claude, 0o755);

  // The entry this release writes, with the install id on it, beside a server
  // of somebody else's that has to survive.
  fs.writeFileSync(
    path.join(home, ".cursor", "mcp.json"),
    `${JSON.stringify(
      {
        mcpServers: {
          other: { url: "https://example.test/mcp" },
          carrick: { url: MCP_URL, headers: { [INSTALL_ID_HEADER]: INSTALL_ID } },
        },
      },
      null,
      2,
    )}\n`,
  );

  fs.mkdirSync(path.dirname(installIdPath(home)), { recursive: true, mode: 0o700 });
  fs.writeFileSync(installIdPath(home), `${INSTALL_ID}\n`, { mode: 0o600 });

  // A session record the Stop hook would have spoken from (carrick#1330). It
  // shares `~/.carrick` with the install id, so leaving it behind also leaves
  // that directory behind.
  fs.mkdirSync(sessionsDir(home), { recursive: true, mode: 0o700 });
  fs.writeFileSync(
    sessionFile("a-session", home)!,
    `${JSON.stringify({ found: [{ name: "slugify", file: "src/util.ts", indexCommit: "abc" }], nudged: [], updated: "" })}\n`,
    { mode: 0o600 },
  );

  const credentials = path.join(root, "config", "carrick");
  fs.mkdirSync(credentials, { recursive: true, mode: 0o700 });
  fs.writeFileSync(
    path.join(credentials, "credentials.json"),
    `${JSON.stringify({ api_base: "https://api.carrick.tools", token: "t", workspace_slug: "acme", obtained_at: "", scope: "cli" }, null, 2)}\n`,
    { mode: 0o600 },
  );

  fs.mkdirSync(path.join(workspace, ".carrick"), { recursive: true });
  fs.writeFileSync(path.join(workspace, ".carrick", "proposal.json"), "{}\n");
  fs.mkdirSync(path.join(workspace, ".github", "workflows"), { recursive: true });
  fs.writeFileSync(path.join(workspace, ".github", "workflows", "carrick.yml"), "name: carrick\n");
  fs.mkdirSync(path.join(workspace, ".claude", "skills", "carrick"), { recursive: true });
  fs.writeFileSync(path.join(workspace, ".claude", "skills", "carrick", "SKILL.md"), "# Carrick\n");
  fs.writeFileSync(path.join(workspace, "carrick.json"), "{}\n");
  fs.writeFileSync(path.join(workspace, "AGENTS.md"), "# Repo\n\n## Carrick\n\nAsk Carrick first.\n");
  // The second settings file this workspace holds has hooks of somebody
  // else's and none of ours, in a format the merge does not write: the run
  // must leave it exactly as it is and say nothing about it.
  fs.writeFileSync(
    path.join(workspace, SETTINGS_FILES[1]!),
    '{\n    "hooks": {\n        "PostToolUse": [\n            { "hooks": [{ "type": "command", "command": "eslint --fix" }] }\n        ]\n    }\n}',
  );
  fs.writeFileSync(
    path.join(workspace, SETTINGS_FILES[0]!),
    mergeCarrickHooks(
      JSON.stringify({
        permissions: { allow: ["Bash(ls:*)"] },
        hooks: {
          SessionStart: [{ hooks: [{ type: "command", command: '"$CLAUDE_PROJECT_DIR/.claude/session-start.sh"' }] }],
        },
      }),
    ).body,
  );

  const env: NodeJS.ProcessEnv = {
    ...process.env,
    HOME: home,
    USERPROFILE: home,
    XDG_CONFIG_HOME: path.join(root, "config"),
    PATH: `${bin}${path.delimiter}${process.env["PATH"] ?? ""}`,
  };
  delete env["CARRICK_TOKEN"];
  delete env["CARRICK_BIN"];
  delete env["CI"];
  return { root, home, workspace, env, log };
}

const posixFixture = { skip: process.platform === "win32" ? "the fixture client needs a POSIX shebang" : false };

test("remove takes back what init wrote, lists what it will not touch, and says so once", posixFixture, (t) => {
  const state = machine();
  t.after(() => fs.rmSync(state.root, { recursive: true, force: true }));

  const run = (): { status: number | null; stdout: string; stderr: string } =>
    spawnSync(process.execPath, [path.join(packageRoot, "bin", "carrick.mjs"), "remove", "--workspace", state.workspace], {
      encoding: "utf8",
      env: state.env,
    });

  const first = run();
  assert.equal(first.status, 0, first.stderr);
  assert.equal(first.stderr, "");
  const settingsFile = path.join(state.workspace, SETTINGS_FILES[0]!);
  for (const line of [
    `◇ Carrick hook entries removed from ${SETTINGS_FILES[0]}`,
    "◇ MCP server removed for Claude Code",
    `◇ MCP server removed for Cursor: ${path.join(state.home, ".cursor", "mcp.json")}`,
    "◇ This machine's install id removed",
    "◇ .carrick removed, with the proposal and the index in it",
    "◇ Signed out: the saved credential is gone",
    `  git rm ${[".github/workflows/carrick.yml", ".claude/skills/carrick/SKILL.md", "carrick.json"].join(" ")}`,
    '  by hand — AGENTS.md: the "## Carrick" section',
    "  npm uninstall -g carrick",
  ]) {
    assert.ok(first.stdout.includes(line), `missing from the output: ${line}\n${first.stdout}`);
  }

  // The file with none of our entries is byte for byte what it was, and no
  // line claims anything was removed from it.
  const untouched = path.join(state.workspace, SETTINGS_FILES[1]!);
  assert.equal(
    fs.readFileSync(untouched, "utf8"),
    '{\n    "hooks": {\n        "PostToolUse": [\n            { "hooks": [{ "type": "command", "command": "eslint --fix" }] }\n        ]\n    }\n}',
  );
  assert.equal(first.stdout.includes(SETTINGS_FILES[1]!), false, first.stdout);

  // The hook pack and the permissions survive; only our entries are gone.
  const settings = JSON.parse(fs.readFileSync(settingsFile, "utf8"));
  assert.deepEqual(settings.permissions, { allow: ["Bash(ls:*)"] });
  assert.equal(settings.hooks.SessionStart.length, 1);
  assert.match(settings.hooks.SessionStart[0].hooks[0].command, /session-start\.sh/);
  assert.equal(settings.hooks.PostToolUse, undefined);
  assert.equal(settings.hooks.Stop, undefined);

  // The client's own file keeps the server that is not ours, and the header
  // went with the entry it was on.
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(state.home, ".cursor", "mcp.json"), "utf8")), {
    mcpServers: { other: { url: "https://example.test/mcp" } },
  });
  // The id itself is gone, and the directory it was alone in with it: the
  // next `carrick init` is a new install (carrick-cloud#890).
  assert.equal(fs.existsSync(installIdPath(state.home)), false);
  // The session records go with it: they are a scratch note about
  // conversations that have ended (carrick#1330).
  assert.match(first.stdout, /1 session record\(s\) removed/);
  assert.equal(fs.existsSync(sessionsDir(state.home)), false);
  assert.equal(fs.existsSync(path.join(state.home, ".carrick")), false);
  assert.deepEqual(fs.readFileSync(state.log, "utf8").trim().split("\n"), [
    "mcp get carrick",
    "mcp remove --scope user carrick",
  ]);

  assert.equal(fs.existsSync(path.join(state.workspace, ".carrick")), false);
  assert.equal(fs.existsSync(path.join(state.root, "config", "carrick", "credentials.json")), false);
  // Nothing tracked was touched: every scaffold file is still there, which is
  // why they are printed rather than deleted.
  for (const relative of [".github/workflows/carrick.yml", ".claude/skills/carrick/SKILL.md", "carrick.json", "AGENTS.md"]) {
    assert.ok(fs.existsSync(path.join(state.workspace, relative)), relative);
  }

  const before = fs.readFileSync(settingsFile, "utf8");
  const second = run();
  assert.equal(second.status, 0, second.stderr);
  assert.ok(second.stdout.includes("Nothing left to remove on this machine."));
  assert.equal(second.stdout.includes("◇"), false, second.stdout);
  // The repo listing is advice, not a removal, so it is printed both times.
  assert.ok(second.stdout.includes("git rm "));
  assert.equal(fs.readFileSync(settingsFile, "utf8"), before);
  assert.deepEqual(fs.readFileSync(state.log, "utf8").trim().split("\n"), [
    "mcp get carrick",
    "mcp remove --scope user carrick",
    "mcp get carrick",
  ]);
});

test("--keep-login removes everything else and leaves the credential", posixFixture, (t) => {
  const state = machine();
  t.after(() => fs.rmSync(state.root, { recursive: true, force: true }));
  const result = spawnSync(
    process.execPath,
    [path.join(packageRoot, "bin", "carrick.mjs"), "remove", "--workspace", state.workspace, "--keep-login"],
    { encoding: "utf8", env: state.env },
  );
  assert.equal(result.status, 0, result.stderr);
  assert.ok(result.stdout.includes("This machine stays signed in"));
  assert.equal(result.stdout.includes("Signed out"), false);
  // The install id is not a login: it goes whichever way this flag points.
  assert.equal(fs.existsSync(installIdPath(state.home)), false);
  assert.ok(fs.existsSync(path.join(state.root, "config", "carrick", "credentials.json")));
  assert.equal(fs.existsSync(path.join(state.workspace, ".carrick")), false);
});
