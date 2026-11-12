// Edit-time facts for writes the call itself does not describe: a shell write
// (`sed -i`, a heredoc), a `Write` over an existing file. Their changed lines
// are the file's diff against HEAD (`src/facts/diff.ts`); where that is not
// known (no git, a diff over the cap) nothing is said.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fakeEnv, firstCall, fixturePath, runHook, testDir } from "./helpers.ts";
import { buildFacts, factsPath } from "../src/facts/store.ts";
import { changesAgainstHead, MAX_DIFF_LINES } from "../src/facts/diff.ts";

const WIRE = fixturePath("edit-facts.s1.ndjson");
const REPOS = ["orders-api", "web-app", "billing-worker", "gateway", "billing-api"];

type Ws = { root: string; home: string; at: (rel: string) => string };

const header = JSON.stringify({
  schema: 1,
  writer: 1,
  project_id: "p",
  project_slug: "demo",
  digest: "d",
  built_at: "2026-01-01T00:00:00Z",
  repos: REPOS.map((repo) => ({ repo, service: repo, commit: "1" })),
  counts: { rows: 0, producers: 0, consumers: 0 },
});

const row = (fields: Record<string, unknown>) =>
  JSON.stringify({ k: "op", service: fields["repo"], others: [], total: 0, source: "handler", side: "consumer", protocol: "http", repo: "orders-api", ...fields });

/** The fixture repos in a fresh workspace, the fact file built from the wire fixture plus `extra` rows. */
async function workspace(t: { after: (fn: () => void) => void }, extra: string[] = [], files: Record<string, string> = {}): Promise<Ws> {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-shell-")));
  const home = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-shell-home-")));
  t.after(() => {
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(home, { recursive: true, force: true });
  });
  fs.cpSync(path.join(testDir, "fixtures", "facts"), root, { recursive: true });
  for (const [rel, text] of Object.entries(files)) fs.writeFileSync(path.join(root, rel), text);
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: "demo" }] } }));
  const wire = fs.readFileSync(WIRE, "utf8");
  const body = [header, ...wire.split("\n").slice(1).filter((line) => line.trim() !== ""), ...extra].join("\n");
  await buildFacts(`${body}\n`, factsPath(root), REPOS.map((repo) => ({ repo, path: path.join(root, repo) })), new Date().toISOString());
  return { root, home, at: (rel) => path.join(root, rel) };
}

function commit(dir: string): void {
  for (const args of [["init", "-q"], ["add", "."], ["commit", "-q", "-m", "init"]]) {
    const run = spawnSync("git", ["-C", dir, "-c", "user.email=t@example.com", "-c", "user.name=t", ...args], { encoding: "utf8" });
    assert.equal(run.status, 0, run.stderr);
  }
}

const env = (ws: Ws, extra: Record<string, string> = {}) =>
  fakeEnv({ HOME: ws.home, CLAUDE_PROJECT_DIR: ws.root, CARRICK_FAKE_FIXTURE: fixturePath("check-silent.json"), ...extra });

function shell(ws: Ws, command: string, session: string, cwd: string, extra: Record<string, string> = {}) {
  return runHook("post-bash.ts", {
    payload: { hook_event_name: "PostToolUse", tool_name: "Bash", session_id: session, cwd, tool_input: { command }, tool_response: { stdout: "" } },
    env: env(ws, extra),
  });
}

function context(stdout: string): string {
  if (!stdout.trim()) return "";
  return (JSON.parse(stdout) as { hookSpecificOutput: { additionalContext: string } }).hookSpecificOutput.additionalContext;
}

const TWO = ["export function f() {", "  return 1;", "}", "", "export function g() {", "  return 2;", "}", ""].join("\n");
const TWO_ROWS = [
  row({ label: "POST /f", file: "src/two.ts", line: 1, name: "f", text: "f line" }),
  row({ label: "POST /g", file: "src/two.ts", line: 5, name: "g", text: "g line" }),
];

test("a `sed -i` on a handler prints the same line an Edit would", async (t) => {
  const ws = await workspace(t);
  commit(ws.at("orders-api"));
  const file = ws.at("orders-api/src/routes/orders.ts");
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace('res.status(404).json({ error: "not found" });', 'res.status(410).json({ error: "not found" });'));
  const sed = context((await shell(ws, `sed -i 's/404/410/' ${file}`, "sed", ws.at("orders-api"))).stdout);
  const edit = await runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: "edit",
      cwd: ws.root,
      tool_input: { file_path: file, old_string: "x", new_string: 'res.status(410).json({ error: "not found" });' },
    },
    env: env(ws),
  });
  assert.match(sed, /^GET \/orders\/:id is called at /);
  assert.equal(sed, context(edit.stdout));
});

test("a heredoc that rewrites a file prints only the row of the function whose lines changed", async (t) => {
  const ws = await workspace(t, TWO_ROWS, { "orders-api/src/two.ts": TWO });
  commit(ws.at("orders-api"));
  const file = ws.at("orders-api/src/two.ts");
  const rewritten = TWO.replace("return 2;", "return 3;");
  fs.writeFileSync(file, rewritten);
  const command = `cat > ${file} <<'EOF'\n${rewritten}EOF`;
  assert.equal(context((await shell(ws, command, "heredoc", ws.at("orders-api"))).stdout), "g line");
});

test("a Write over an existing file prints the row of the function it changed and no other", async (t) => {
  const ws = await workspace(t, TWO_ROWS, { "orders-api/src/two.ts": TWO });
  commit(ws.at("orders-api"));
  const file = ws.at("orders-api/src/two.ts");
  const content = TWO.replace("return 1;", "return 10;");
  fs.writeFileSync(file, content);
  const run = await runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Write",
      session_id: "write-over",
      cwd: ws.root,
      tool_input: { file_path: file, content },
      tool_response: { type: "update", filePath: file },
    },
    env: env(ws),
  });
  assert.equal(context(run.stdout), "f line");
});

test("a file outside any git repo is silent", async (t) => {
  const ws = await workspace(t);
  const file = ws.at("orders-api/src/routes/orders.ts");
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace("404", "410"));
  const command = `sed -i 's/404/410/' ${file}`;
  assert.equal(context((await shell(ws, command, "no-git", ws.at("orders-api"))).stdout), "");
  assert.equal(changesAgainstHead(file, true), null);
  // The same write once the folder is a repository with the old text committed: the line prints.
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace("410", "404"));
  commit(ws.at("orders-api"));
  fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace("404", "410"));
  assert.match(context((await shell(ws, command, "with-git", ws.at("orders-api"))).stdout), /^GET \/orders\/:id is called at /);
});

test("a diff over the cap is silent", async (t) => {
  const filler = (word: string) => Array.from({ length: MAX_DIFF_LINES + 500 }, (_, i) => `// ${word} ${i}`).join("\n");
  const ws = await workspace(t, TWO_ROWS, { "orders-api/src/two.ts": `${TWO}${filler("old")}\n` });
  commit(ws.at("orders-api"));
  const file = ws.at("orders-api/src/two.ts");
  // Every filler line changes, and so does g: over the cap, nothing is said.
  const big = `${TWO.replace("return 2;", "return 3;")}${filler("new")}\n`;
  fs.writeFileSync(file, big);
  const command = `cat > ${file} <<'EOF'\n${big}EOF`;
  assert.equal(context((await shell(ws, command, "big", ws.at("orders-api"))).stdout), "");
  assert.equal(changesAgainstHead(file, false), null);
  // Control: the same file with only g changed prints.
  fs.writeFileSync(file, `${TWO.replace("return 2;", "return 3;")}${filler("old")}\n`);
  assert.equal(context((await shell(ws, command, "small", ws.at("orders-api"))).stdout), "g line");
});

test("a `.mjs` file's shell write is seen by the fact path and starts no probe", async (t) => {
  const mjs = "export function h() {\n  return 1;\n}\n";
  const ws = await workspace(t, [row({ label: "POST /h", file: "src/h.mjs", line: 1, name: "h", text: "h line" })], { "orders-api/src/h.mjs": mjs });
  commit(ws.at("orders-api"));
  const file = ws.at("orders-api/src/h.mjs");
  fs.writeFileSync(file, mjs.replace("return 1;", "return 2;"));
  const argvLog = path.join(ws.home, "argv.log");
  const run = await shell(ws, `sed -i 's/1/2/' ${file}`, "mjs", ws.at("orders-api"), { CARRICK_FAKE_ARGV_LOG: argvLog });
  assert.equal(context(run.stdout), "h line");
  assert.equal(firstCall(argvLog), null);
  // The Edit tool on the same file: facts only, no probe.
  fs.writeFileSync(file, mjs.replace("return 1;", "return 3;"));
  const edit = await runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: "mjs-edit",
      cwd: ws.root,
      tool_input: { file_path: file, old_string: "x", new_string: "return 3;" },
    },
    env: env(ws, { CARRICK_FAKE_ARGV_LOG: argvLog }),
  });
  assert.equal(context(edit.stdout), "h line");
  assert.equal(firstCall(argvLog), null);
});

test("an Edit whose hunks went stale after a formatter ran falls back to the diff against HEAD", async (t) => {
  const ws = await workspace(t, TWO_ROWS, { "orders-api/src/two.ts": TWO });
  commit(ws.at("orders-api"));
  const file = ws.at("orders-api/src/two.ts");
  // The tool's patch put the change at line 2; a formatter then moved it, so
  // the file now holds it at line 6 and the patch's line does not match.
  fs.writeFileSync(file, TWO.replace("return 2;", "return 3;"));
  const run = await runHook("post-edit.ts", {
    payload: {
      hook_event_name: "PostToolUse",
      tool_name: "Edit",
      session_id: "stale-hunks",
      cwd: ws.root,
      tool_input: { file_path: file, old_string: "return 2;", new_string: "return 3;" },
      tool_response: { filePath: file, structuredPatch: [{ oldStart: 2, oldLines: 1, newStart: 2, newLines: 1, lines: ["-  return 1;", "+  return 3;"] }] },
    },
    env: env(ws),
  });
  assert.equal(context(run.stdout), "g line");
});
