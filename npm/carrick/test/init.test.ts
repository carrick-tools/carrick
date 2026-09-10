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
import { writeConfigs, type WorkspaceProposal } from "../src/init/repos.ts";
import {
  carrickHooks,
  hookCommand,
  mergeCarrickHooks,
  ownEntryPoint,
} from "../src/init/settings.ts";
import { editorLines, parseArgs, init } from "../src/init/run.ts";

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
  const parsed = parseArgs(["-y", "--workspace", "/code"], "/tmp");
  assert.deepEqual(parsed, { workspace: "/code", assumeYes: true, skipIndex: false });
  assert.equal((parseArgs(["/code"], "/tmp") as { workspace: string }).workspace, "/code");
  assert.match(parseArgs(["--nope"]) as string, /unknown option/);
});
