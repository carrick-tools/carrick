// What `carrick init` decides before it writes anything.
//
// The two that matter most are the two that can destroy something: the repo
// list a user edited by hand, and a settings file that already holds someone
// else's hooks. Both are merges, and both are tested for what they leave alone.

import assert from "node:assert/strict";
import test from "node:test";
import { describeIdentity, githubIdentity, loginFromGhStatus } from "../src/init/identity.ts";
import { findRepos, mergeWorkspace } from "../src/init/repos.ts";
import {
  carrickHooks,
  hookCommand,
  mergeCarrickHooks,
  ownEntryPoint,
} from "../src/init/settings.ts";
import { parseArgs } from "../src/init/run.ts";

function entries(names: Array<[string, boolean]>) {
  return names.map(([name, isDirectory]) => ({ name, isDirectory: () => isDirectory }));
}

test("a repo is a directory here with a package.json in it", () => {
  const found = findRepos("/ws", {
    readdir: () =>
      entries([
        ["api", true],
        ["web", true],
        ["node_modules", true],
        [".git", true],
        ["dist", true],
        ["notes", true],
        ["README.md", false],
      ]),
    exists: (target) => !target.includes("notes"),
  });
  assert.deepEqual(found, ["./api", "./web"]);
});

test("the repo list keeps what a user put there, in the order they put it", () => {
  const existing = JSON.stringify({ repos: ["./web", "../shared-client"] }, null, 2);
  const merged = mergeWorkspace(existing, ["./api", "./web"]);
  assert.deepEqual(merged.repos, ["./web", "../shared-client", "./api"]);
  assert.deepEqual(merged.added, ["./api"]);
});

test("running init twice writes the same repo list", () => {
  const first = mergeWorkspace(null, ["./api", "./web"]);
  const second = mergeWorkspace(first.body, ["./api", "./web"]);
  assert.equal(second.body, first.body);
  assert.deepEqual(second.added, []);
});

test("a trailing slash or a missing ./ is the same repo, not a second one", () => {
  const existing = JSON.stringify({ repos: ["api/", "./web"] }, null, 2);
  const merged = mergeWorkspace(existing, ["./api", "./web"]);
  assert.deepEqual(merged.added, []);
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

test("the identity comes from the GitHub CLI, or a token, or init stops", () => {
  const gh = githubIdentity({ ghStatus: () => "  ✓ Logged in to github.com account octocat (keyring)" });
  assert.deepEqual(gh.identity, { login: "octocat", source: "gh" });
  assert.match(describeIdentity(gh.identity!), /octocat/);

  const older = loginFromGhStatus("✓ Logged in to github.com as octocat (oauth_token)");
  assert.equal(older, "octocat");

  const token = githubIdentity({
    env: { GITHUB_TOKEN: "x" },
    ghStatus: () => {
      throw new Error("gh: not found");
    },
  });
  assert.equal(token.identity?.source, "token");

  const none = githubIdentity({
    env: {},
    ghStatus: () => {
      throw new Error("gh: not found");
    },
  });
  assert.equal(none.identity, null);
  assert.match(none.problem ?? "", /gh auth login/);
  assert.match(none.problem ?? "", /GITHUB_TOKEN/);
});

test("init reads its arguments", () => {
  const parsed = parseArgs(["-y", "--workspace", "/code"], "/tmp");
  assert.deepEqual(parsed, { workspace: "/code", assumeYes: true, skipIndex: false });
  assert.equal((parseArgs(["/code"], "/tmp") as { workspace: string }).workspace, "/code");
  assert.match(parseArgs(["--nope"]) as string, /unknown option/);
});
