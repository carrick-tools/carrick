// What `carrick doctor` reports, and — more importantly — what it does not.
//
// Exit is non-zero on any finding, so every check here has a pair: the shape a
// healthy repo has, which must produce no finding, and the shape that is
// actually broken, which must. The healthy half is the half that matters. A
// doctor that fires on a workflow someone added a deploy step to, or on a
// monorepo whose tsconfig sits inside its service directory, is a command
// people turn off.
//
// Every test states its own workspace, and the executable one states its own
// HOME, so nothing here can read or write the machine it runs on.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
  checkDeclaredPaths,
  checkHooks,
  checkIndex,
  checkMcp,
  checkTaskSkills,
  checkWorkflow,
  configuredRepos,
  declaredServices,
  findingCount,
  functionalLines,
  parseArgs,
  templateDrift,
  workflowVariables,
  type GitReader,
  type HookMachine,
  type Line,
} from "../src/init/doctor.ts";
import { expectedCarrickHooks, installedCarrickHooks, mergeCarrickHooks } from "../src/init/settings.ts";
import {
  inspectTaskSkills,
  skillFile,
  SKILL_ROOTS,
  stamped,
  writeTaskSkills,
} from "../src/init/task-skills.ts";
import { renderTemplate, TEMPLATE_PATHS } from "../src/templates.ts";
import { statusFixture } from "./helpers.ts";
import type { StatusResult } from "../src/contract.ts";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const workflowPath = TEMPLATE_PATHS.workflow;

/** A throwaway workspace, one repo, with whatever files a test names. */
function workspace(files: Record<string, string>): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-doctor-")));
  for (const [relative, body] of Object.entries(files)) {
    const target = path.join(root, relative);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, body);
  }
  return root;
}

function texts(lines: Line[]): string[] {
  return lines.map((line) => `${line.level}: ${line.text}`);
}

test("doctor reads its arguments", () => {
  assert.deepEqual(parseArgs([], "/work"), { workspace: "/work" });
  assert.deepEqual(parseArgs(["repo"], "/work"), { workspace: "/work/repo" });
  assert.deepEqual(parseArgs(["-w", "/elsewhere"], "/work"), { workspace: "/elsewhere" });
  assert.match(parseArgs(["--workspace"], "/work") as string, /needs a directory/);
  assert.match(parseArgs(["--nonsense"], "/work") as string, /unknown option/);
  assert.match(parseArgs(["--help"], "/work") as string, /^carrick doctor/);
});

test("a config whose paths are all there is one line and no finding", () => {
  // The tsconfig is INSIDE the service directory, which is where the scanner
  // resolves it from: a doctor that joined it to the repo root would report a
  // missing file on every monorepo that declares one.
  const root = workspace({
    "carrick.json": JSON.stringify({
      services: [
        { serviceName: "api", directory: "api", tsconfig: "tsconfig.json", include: ["packages/shared"] },
        { serviceName: "web", directory: "web" },
      ],
    }),
    "api/tsconfig.json": "{}",
    "api/src/index.ts": "export {};\n",
    "web/index.ts": "export {};\n",
    "packages/shared/index.ts": "export {};\n",
  });
  const lines = checkDeclaredPaths(configuredRepos(root));
  assert.equal(findingCount(lines), 0, texts(lines).join("\n"));
  assert.deepEqual(texts(lines), ["done: Declared paths exist: 4 in 1 carrick.json file(s)."]);
  fs.rmSync(root, { recursive: true, force: true });
});

test("every kind of path a config can declare and not have is named", () => {
  const root = workspace({
    "carrick.json": JSON.stringify({
      serviceName: "api",
      directory: "api",
      tsconfig: "tsconfig.json",
      include: ["packages/shared"],
    }),
    "api/src/index.ts": "export {};\n",
  });
  const lines = checkDeclaredPaths(configuredRepos(root));
  assert.equal(findingCount(lines), 2, texts(lines).join("\n"));
  assert.ok(lines.some((line) => line.text.includes('includes "packages/shared"')));
  assert.ok(lines.some((line) => line.text.includes('declares tsconfig "tsconfig.json"')));
  // The directory itself is there, so it is not among them.
  assert.ok(!lines.some((line) => line.text.includes("declares directory")));
  fs.rmSync(root, { recursive: true, force: true });
});

test("a declared GraphQL schema is found through its glob, even under a build folder, and a missing one is named", () => {
  const root = workspace({
    "carrick.json": JSON.stringify({
      services: [
        {
          serviceName: "api",
          directory: "api",
          graphqlSchemas: ["web/dist/**/*.graphql", "api/schema.graphql"],
        },
        { serviceName: "web", directory: "web" },
      ],
    }),
    "api/src/index.ts": "export {};\n",
    "web/index.ts": "export {};\n",
    "web/dist/graphql/schema.graphql": "type Query { widgets: [String!]! }\n",
  });
  const lines = checkDeclaredPaths(configuredRepos(root));
  assert.equal(findingCount(lines), 1, texts(lines).join("\n"));
  assert.ok(
    lines.some((line) => line.text.includes('declares graphqlSchemas "api/schema.graphql", which matches no file')),
    texts(lines).join("\n"),
  );
  assert.ok(!lines.some((line) => line.text.includes("web/dist/**/*.graphql")));
  fs.rmSync(root, { recursive: true, force: true });
});

test("a missing service directory, a config that is not JSON, and a workspace with no config at all", () => {
  const gone = workspace({ "carrick.json": JSON.stringify({ serviceName: "api", directory: "api" }) });
  assert.match(checkDeclaredPaths(configuredRepos(gone))[0]!.text, /declares directory "api"/);

  // A file that will not parse is not an absent one: one sentence, not two.
  const broken = workspace({ "carrick.json": "{ oops" });
  const brokenLines = checkDeclaredPaths(configuredRepos(broken));
  assert.equal(brokenLines.length, 1, texts(brokenLines).join("\n"));
  assert.equal(brokenLines[0]!.level, "refuse");
  assert.match(brokenLines[0]!.text, /not valid JSON/);

  const bare = workspace({ "src/index.ts": "export {};\n" });
  const bareLines = checkDeclaredPaths(configuredRepos(bare));
  assert.equal(findingCount(bareLines), 1);
  assert.match(bareLines.at(-1)!.text, /No carrick.json anywhere in this workspace/);

  for (const root of [gone, broken, bare]) fs.rmSync(root, { recursive: true, force: true });
});

test("a sibling repo with no carrick.json is reported as a fact, not as a finding", () => {
  const root = workspace({
    "carrick.json": JSON.stringify({ serviceName: "api" }),
    "notes/.git": "gitdir: elsewhere\n",
    "notes/README.md": "not a Carrick repo\n",
  });
  const lines = checkDeclaredPaths(configuredRepos(root));
  assert.equal(findingCount(lines), 0, texts(lines).join("\n"));
  assert.ok(lines.some((line) => line.level === "say" && line.text.includes("notes has no carrick.json")));
  fs.rmSync(root, { recursive: true, force: true });
});

test("the services a config declares, flat and as an array", () => {
  assert.deepEqual(declaredServices({ serviceName: "api", directory: "src", include: ["shared"] }), [
    { name: "api", include: ["shared"], graphqlSchemas: [], directory: "src" },
  ]);
  // `services` wins over its sibling flat fields, as `Config::load_services` does.
  assert.deepEqual(
    declaredServices({ serviceName: "ignored", services: [{ name: "api", directory: "api" }] }),
    [{ name: "api", include: [], graphqlSchemas: [], directory: "api" }],
  );
  // A service with nothing to place is one service at the repo root.
  assert.deepEqual(declaredServices({ internalEnvVars: ["API_URL"] }), [{ name: "service 1", include: [], graphqlSchemas: [] }]);
});

test("comments and blank lines are not drift", () => {
  const template = renderTemplate("workflow");
  const stripped = template
    .split("\n")
    .filter((line) => line.trim() !== "" && !line.trim().startsWith("#"))
    .join("\n");
  const drift = templateDrift(stripped, template);
  assert.deepEqual(drift.missing, []);
  assert.deepEqual(drift.added, []);
  assert.ok(functionalLines(template).every((line) => !line.trim().startsWith("#")));
});

test("a diff names the template lines that are gone and the lines that are yours", () => {
  const template = renderTemplate("workflow");
  const withoutDispatch = template
    .split("\n")
    .filter((line) => !line.includes("repository_dispatch"))
    .join("\n");
  const missing = templateDrift(withoutDispatch, template);
  assert.deepEqual(missing.missing, ["  repository_dispatch:"]);
  assert.ok(missing.diff.includes("-   repository_dispatch:"));

  const withStep = `${template}\n      - run: ./deploy.sh\n`;
  const added = templateDrift(withStep, template);
  assert.deepEqual(added.missing, []);
  assert.deepEqual(added.added, ["      - run: ./deploy.sh"]);
});

test("a workflow's own action ref and branch are read back, so neither is drift", () => {
  const pinned = renderTemplate("workflow", { ACTION_REF: "carrick-tools/carrick@v1.4.2", DEFAULT_BRANCH: "trunk" });
  assert.deepEqual(workflowVariables(pinned), {
    variables: { ACTION_REF: "carrick-tools/carrick@v1.4.2", DEFAULT_BRANCH: "trunk" },
    unread: [],
  });
  const root = workspace({
    "carrick.json": JSON.stringify({ serviceName: "api" }),
    [workflowPath]: `${pinned}      - run: ./deploy.sh\n`,
  });
  const lines = checkWorkflow(configuredRepos(root));
  assert.equal(findingCount(lines), 0, texts(lines).join("\n"));
  assert.ok(lines.some((line) => line.text.includes("plus 1 line(s) of your own")));
  fs.rmSync(root, { recursive: true, force: true });

  // A branch list written as a YAML sequence cannot be read, and the line says so.
  const sequence = renderTemplate("workflow").replace(/branches: \[main\]/g, "branches:\n      - main");
  assert.deepEqual(workflowVariables(sequence).unread, ["DEFAULT_BRANCH"]);
});

test("a repo with no workflow, and one the current template has grown past", () => {
  const none = workspace({ "carrick.json": JSON.stringify({ serviceName: "api" }) });
  const noneLines = checkWorkflow(configuredRepos(none));
  assert.equal(findingCount(noneLines), 1);
  assert.match(noneLines[0]!.text, /no \.github\/workflows\/carrick\.yml/);

  const behind = workspace({
    "carrick.json": JSON.stringify({ serviceName: "api" }),
    [workflowPath]: renderTemplate("workflow")
      .split("\n")
      .filter((line) => !line.includes("repository_dispatch") && !line.includes("carrick-sibling-updated"))
      .join("\n"),
  });
  const behindLines = checkWorkflow(configuredRepos(behind));
  assert.equal(findingCount(behindLines), 1);
  assert.match(behindLines[0]!.text, /missing 2 line\(s\) of the current template/);
  assert.match(behindLines[0]!.text, /- {3}repository_dispatch:/);

  // A repo Carrick does not index is not asked for a workflow.
  const unconfigured = workspace({ "src/index.ts": "export {};\n" });
  assert.deepEqual(checkWorkflow(configuredRepos(unconfigured)), []);

  for (const root of [none, behind, unconfigured]) fs.rmSync(root, { recursive: true, force: true });
});

/** A machine where `carrick` is wherever the test says it is. */
function hookMachine(overrides: Partial<HookMachine> = {}): HookMachine {
  return {
    resolveOnPath: () => null,
    realpath: (target) => target,
    entryPoint: "/usr/lib/node_modules/carrick/bin/carrick.mjs",
    ...overrides,
  };
}

function withHooks(command: string | null): string {
  const root = workspace({});
  if (command !== null) {
    fs.mkdirSync(path.join(root, ".claude"), { recursive: true });
    fs.writeFileSync(path.join(root, ".claude", "settings.json"), mergeCarrickHooks(null, command).body);
  }
  return root;
}

test("hooks that are installed, current, and run this package", () => {
  const root = withHooks("carrick");
  const lines = checkHooks(
    root,
    hookMachine({ resolveOnPath: () => "/usr/lib/node_modules/carrick/bin/carrick.mjs" }),
  );
  assert.equal(findingCount(lines), 0, texts(lines).join("\n"));
  assert.match(lines[0]!.text, /^Agent hooks are installed here and run this package/);
  fs.rmSync(root, { recursive: true, force: true });
});

test("an install PATH reaches through a shim, and a doctor run under npx, prove resolution and stop", () => {
  // A pnpm global install and every Windows install put a shim on PATH, and a
  // shim does not realpath to an entry point. Comparing it to this package's
  // would report two installs on a machine that has one, and the command exits
  // non-zero on any finding, so that is every run for those users.
  const shim = withHooks("carrick");
  const shimLines = checkHooks(
    shim,
    hookMachine({ resolveOnPath: () => "/home/dev/.local/share/pnpm/carrick" }),
  );
  assert.equal(findingCount(shimLines), 0, texts(shimLines).join("\n"));
  assert.match(shimLines[0]!.text, /resolves to \/home\/dev\/\.local\/share\/pnpm\/carrick\.$/);

  // `npx carrick doctor` makes the transient copy the one asking. The hooks
  // are pointing at the real install; blaming them for it inverts the finding.
  const underNpx = withHooks("carrick");
  const npxLines = checkHooks(
    underNpx,
    hookMachine({
      resolveOnPath: () => "/usr/lib/node_modules/carrick/bin/carrick.mjs",
      entryPoint: "/home/dev/.npm/_npx/abc123/node_modules/carrick/bin/carrick.mjs",
    }),
  );
  assert.equal(findingCount(npxLines), 0, texts(npxLines).join("\n"));
  assert.match(npxLines[0]!.text, /cannot say whether that is the same one/);

  for (const root of [shim, underNpx]) fs.rmSync(root, { recursive: true, force: true });
});

test("the ways a hook entry stops working, each named", () => {
  const missing = workspace({});
  assert.match(checkHooks(missing, hookMachine())[0]!.text, /No \.claude settings in this folder/);

  const empty = workspace({ ".claude/settings.json": '{ "permissions": { "allow": [] } }' });
  assert.match(checkHooks(empty, hookMachine())[0]!.text, /No Carrick hook entries/);

  // Written when `carrick` was on PATH, run on a machine where it is not.
  const gone = withHooks("carrick");
  const goneLines = checkHooks(gone, hookMachine());
  assert.equal(goneLines[0]!.level, "refuse");
  assert.match(goneLines[0]!.text, /does not resolve on PATH, so every edit fails silently/);

  // An npx install resolves for the length of one command and never again.
  const transient = withHooks("carrick");
  const npx = path.join("/home", "dev", ".npm", "_npx", "abc123", "node_modules", "carrick", "bin", "carrick.mjs");
  const npxLines = checkHooks(transient, hookMachine({ resolveOnPath: () => npx }));
  assert.equal(npxLines[0]!.level, "warn");
  assert.match(npxLines[0]!.text, /temporary npx install/);

  // A second install answers for the machine and the hooks use the other one.
  const other = withHooks("carrick");
  const otherLines = checkHooks(other, hookMachine({ resolveOnPath: () => "/opt/old/carrick/bin/carrick.mjs" }));
  assert.equal(otherLines[0]!.level, "warn");
  assert.match(otherLines[0]!.text, /Two installs answer for one machine/);

  // An absolute entry whose file has been uninstalled.
  const stale = withHooks("/opt/old/carrick/bin/carrick.mjs");
  const staleLines = checkHooks(stale, hookMachine({ realpath: () => null }));
  assert.equal(staleLines[0]!.level, "refuse");
  assert.match(staleLines[0]!.text, /is not a file on this machine/);

  const broken = workspace({ ".claude/settings.json": "{ oops" });
  assert.equal(checkHooks(broken, hookMachine())[0]!.level, "refuse");

  for (const root of [missing, empty, gone, transient, other, stale, broken]) {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("a settings file holding last version's entries is a finding, not a pass", () => {
  // The entries are ours by every test `isOurs` applies, and the SessionStart
  // one this version writes is not there: "present" is not "current".
  const settings = (matcher: string): string =>
    JSON.stringify(
      {
        hooks: {
          PostToolUse: [
            { matcher, hooks: [{ type: "command", command: "carrick hook post-edit", timeout: 15 }] },
          ],
        },
      },
      null,
      2,
    );
  const onPath = hookMachine({ resolveOnPath: () => "/usr/lib/node_modules/carrick/bin/carrick.mjs" });

  const noSessionStart = workspace({ ".claude/settings.json": settings("Write|Edit|MultiEdit") });
  const lines = checkHooks(noSessionStart, onPath);
  assert.equal(findingCount(lines), 1, texts(lines).join("\n"));
  // Both of the entries that settings file lacks are named, so a check that
  // learns a new entry (the Stop nudge, carrick#1330) reports it without a
  // line of its own here.
  assert.match(lines[0]!.text, /not the ones this version installs: SessionStart/);
  assert.match(lines[0]!.text, /Stop `carrick hook stop`/);

  // A matcher that no longer covers every editing tool is the same failure:
  // the entry is ours, it runs, and it never fires on a MultiEdit.
  const oldMatcher = workspace({ ".claude/settings.json": settings("Write|Edit") });
  const matcherLines = checkHooks(oldMatcher, onPath);
  assert.equal(findingCount(matcherLines), 1);
  assert.match(matcherLines[0]!.text, /PostToolUse .+ \(matcher Write\|Edit\|MultiEdit\)/);

  for (const root of [noSessionStart, oldMatcher]) fs.rmSync(root, { recursive: true, force: true });
});

// carrick#1331 item 1, and carrick#1333: doctor could not see the eight skill
// files at all, and an upgrade leaves them behind silently. Three different
// things with three different answers, so a reader knows which of their files
// `carrick init` will rewrite and which it will not touch.
test("missing skills, skills an older version wrote, and skills edited here", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-doctor-skills-"));
  try {
    writeTaskSkills(dir, { slug: "acme-index" });
    // Current and untouched: no finding, and one line saying so.
    const healthy = checkTaskSkills(inspectTaskSkills(dir));
    assert.equal(findingCount(healthy), 0, texts(healthy).join("\n"));
    assert.match(healthy[0]!.text, /Task skills installed here and current/);

    // One body an older release rendered, stamped as that release stamped it.
    const stale = path.join(dir, skillFile(SKILL_ROOTS[0]!, "carrick-drift"));
    const body = fs.readFileSync(stale, "utf8").split("\n<!-- carrick:skill")[0]!;
    fs.writeFileSync(stale, stamped(`${body}\nA step an older version had.\n`));
    // One somebody here has changed.
    fs.appendFileSync(path.join(dir, skillFile(SKILL_ROOTS[1]!, "carrick-reuse")), "\nOurs.\n");
    // And one deleted.
    fs.rmSync(path.join(dir, skillFile(SKILL_ROOTS[0]!, "carrick-census")));

    const lines = checkTaskSkills(inspectTaskSkills(dir));
    const all = texts(lines).join("\n");
    // Missing and outdated are faults; an edited file is a note, because a
    // team that changed a skill meant to and doctor exits non-zero on faults.
    assert.equal(findingCount(lines), 2, all);
    assert.match(all, /1 of the 8 task skill file\(s\) are missing here/);
    assert.match(all, /1 task skill file\(s\) here were written by an older version of carrick/);
    assert.match(all, /carrick-reuse\/SKILL\.md has been edited here/);
    assert.equal(lines.find((line) => line.text.includes("edited here"))?.level, "say");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("the hook reader sees exactly what the writer wrote", () => {
  for (const command of ["carrick", '"/opt/my tools/carrick/bin/carrick.mjs"']) {
    const body = mergeCarrickHooks(null, command).body;
    assert.deepEqual(installedCarrickHooks(body), expectedCarrickHooks(command));
  }
  // Somebody else's hooks are not ours, whatever they run.
  const others = JSON.stringify({
    hooks: { PostToolUse: [{ hooks: [{ type: "command", command: "eslint --fix" }] }] },
  });
  assert.deepEqual(installedCarrickHooks(others), []);
});

test("an MCP client that is connected, one that is not, and one pointing elsewhere", () => {
  assert.equal(findingCount(checkMcp([{ client: "Cursor", state: "connected", detail: "url" }])), 0);
  assert.deepEqual(texts(checkMcp([])), [
    "say: No agent client on this machine holds an MCP configuration, so there is none to check.",
  ]);
  const mixed = checkMcp([
    { client: "Claude Code", state: "connected", detail: "https://api.carrick.tools/mcp" },
    { client: "Cursor", state: "absent", detail: "no \"carrick\" server in /home/dev/.cursor/mcp.json" },
    { client: "VS Code", state: "elsewhere", detail: '"carrick" in /x/mcp.json points at http://localhost:9000' },
  ]);
  assert.equal(findingCount(mixed), 2);
  assert.match(mixed[0]!.text, /Cursor is on this machine and is not connected/);
  assert.match(mixed[1]!.text, /points at http:\/\/localhost:9000/);
});

// Connected, and answering as nobody in particular: an entry written before
// the install id existed is drift (carrick-cloud#890). The repair travels in
// the detail, because it differs per client — a file client is merged in place
// by `carrick init`, and Claude Code's entry is the owner's to write again.
test("an MCP entry with no install id is a finding, with the repair on the line", () => {
  const pair =
    "MCP entry has no install id. To add it: claude mcp remove --scope user carrick && " +
    'claude mcp add --scope user --transport http carrick https://api.carrick.tools/mcp ' +
    '--header "X-Carrick-Install-Id: 11111111-2222-4333-8444-555555555555"';
  const lines = checkMcp([
    { client: "Cursor", state: "unstamped", detail: "MCP entry has no install id; run carrick init" },
    { client: "Claude Code", state: "unstamped", detail: pair },
  ]);
  assert.equal(findingCount(lines), 2);
  assert.deepEqual(texts(lines), [
    "warn: Cursor: MCP entry has no install id; run carrick init.",
    `warn: Claude Code: ${pair}.`,
  ]);
});

const noGit: GitReader = { defaultRemoteBranch: () => null, commitsBetween: () => null };

test("an index that answers for every service, with the working tree as a note", () => {
  const status = statusFixture("status-workspace.json");
  const enriched: StatusResult = {
    ...status,
    services: status.services.map((service) => ({ ...service, hosted_state: "enriched" as const })),
    repos: [
      { repo: "/workspace/user-service", name: "user-service", changed_since_index: 7, outside_every_service: 0 },
    ],
  };
  const lines = checkIndex(enriched, null, noGit);
  assert.equal(findingCount(lines), 0, texts(lines).join("\n"));
  assert.match(lines[0]!.text, /answers for all 3 indexed service\(s\)/);
  assert.ok(lines.some((line) => line.level === "say" && line.text.includes("7 file(s) have changed")));
});

test("a hosted state that is not enriched is a finding, and says the CLI's own sentence", () => {
  const status = statusFixture("status-workspace.json");
  const broken: StatusResult = {
    ...status,
    services: [
      { ...status.services[0]!, hosted_state: "not_connected", boundary_note: "user-service is not connected to a Carrick project." },
      { ...status.services[1]!, hosted_state: "enriched" },
      { ...status.services[2]!, hosted_state: "enriched" },
    ],
  };
  const lines = checkIndex(broken, null, noGit);
  assert.equal(findingCount(lines), 1);
  assert.equal(
    lines[0]!.text,
    "user-service: user-service is not connected to a Carrick project. (hosted state: not_connected)",
  );
});

test("no index, and a scanner that did not answer", () => {
  assert.match(checkIndex(statusFixture("status-not-indexed.json"), null, noGit)[0]!.text, /not_indexed/);
  const failed = checkIndex(null, "carrick status --json failed: timed out", noGit);
  assert.equal(failed[0]!.level, "refuse");
  assert.match(failed[0]!.text, /Could not read the local index: carrick status --json failed/);
});

test("how far the index is behind the branch CI indexes is a note with the numbers in it", () => {
  const status = statusFixture("status-workspace.json");
  const behind: StatusResult = {
    ...status,
    services: status.services.map((service) => ({ ...service, hosted_state: "enriched" as const })),
    repos: [
      { repo: "/workspace/user-service", name: "user-service", changed_since_index: 0, outside_every_service: 0 },
    ],
  };
  const git: GitReader = { defaultRemoteBranch: () => "origin/main", commitsBetween: () => 4 };
  const lines = checkIndex(behind, null, git);
  assert.equal(findingCount(lines), 0, texts(lines).join("\n"));
  assert.ok(lines.some((line) => line.text.includes("4 commit(s) behind origin/main")));

  const unknown: GitReader = { defaultRemoteBranch: () => "origin/main", commitsBetween: () => null };
  assert.ok(
    checkIndex(behind, null, unknown).some((line) => line.text.includes("a commit this clone does not hold")),
  );
});

test("the whole command, on a workspace with exactly three findings", () => {
  // One repo: an include that is not there, a workflow that matches the
  // template, hooks that run this package, the task skills never written, no
  // agent client, and an index whose hosted half never arrived. Three
  // findings, exit 1, and everything else a line that costs nothing.
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-doctor-home-")));
  const entry = path.join(packageRoot, "bin", "carrick.mjs");
  const root = workspace({
    "carrick.json": JSON.stringify(
      { serviceName: "api", directory: "api", include: ["packages/shared"] },
      null,
      2,
    ),
    "api/index.ts": "export {};\n",
    [workflowPath]: renderTemplate("workflow"),
    ".claude/settings.json": mergeCarrickHooks(null, entry).body,
  });
  const fixture = path.join(root, "status.json");
  fs.writeFileSync(
    fixture,
    JSON.stringify({
      schema: "carrick.status/0",
      workspace: root,
      services: [
        {
          service: "api",
          repo: root,
          index_commit: "6a1b2c3d4e5f60718293a4b5c6d7e8f900112233",
          routes: 3,
          calls: 1,
          changed_since_index: 0,
          hosted_state: "no_index_yet",
          boundary_note: "api is connected and has no hosted index yet. Run `carrick index` once to classify them.",
        },
      ],
    }),
  );

  const run = spawnSync(process.execPath, [path.join(packageRoot, "bin", "carrick.mjs"), "doctor", root], {
    encoding: "utf8",
    env: {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      XDG_CONFIG_HOME: path.join(home, ".config"),
      CLAUDE_CONFIG_DIR: path.join(home, ".claude-config"),
      APPDATA: path.join(home, "AppData"),
      CARRICK_BIN: path.join(packageRoot, "test", "fake-carrick.mjs"),
      CARRICK_FAKE_FIXTURE: fixture,
    },
  });

  assert.equal(run.status, 1, `${run.stdout}\n${run.stderr}`);
  const lines = run.stdout.trimEnd().split("\n");
  assert.deepEqual(lines, [
    `■ ${path.basename(root)}: service "api" includes "packages/shared", which is not a directory in this repo.`,
    `◇ CI workflow matches the current template in 1 repo(s).`,
    `◇ Agent hooks are installed here and run this package (${entry}).`,
    "▲ 8 of the 8 task skill file(s) are missing here (.claude/skills/carrick-impact/SKILL.md and others), so your agent has no Carrick task to follow. `carrick init` writes them.",
    "No agent client on this machine holds an MCP configuration, so there is none to check.",
    "▲ api: api is connected and has no hosted index yet. Run `carrick index` once to classify them. (hosted state: no_index_yet)",
    "",
    "3 finding(s) above, marked ▲ or ■. Nothing here was changed; `carrick doctor` exits non-zero while any of them stand.",
  ]);
  // Read-only: the workspace holds what the test put there and nothing else.
  assert.deepEqual(fs.readdirSync(root).sort(), [".claude", ".github", "api", "carrick.json", "status.json"]);

  for (const target of [root, home]) fs.rmSync(target, { recursive: true, force: true });
});
