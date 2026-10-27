// The Stop hook's second line: source edits that no `carrick check` has seen
// since they were made (carrick#2933).
//
// The measured problem: an agent changed a response shape across ten files and
// called the task done without running `carrick check --changed`. The
// post-edit hook re-checks one file at a time; a change that spans files is
// only judged whole by `--changed`, and nothing asked for it.
//
// What it reads is the session's own transcript (`transcript_path` in the Stop
// payload), not a record the post-edit hook keeps: the transcript already holds
// both halves in order, every Edit/Write/MultiEdit and every Bash command, so
// "edited after the last check" is a walk down one file. Nothing here throws:
// a transcript that is missing, unreadable or in a shape this does not know
// means no line.
//
// Reference: `docs/reference/task-skills.md`, "The reuse nudge".

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { RUN_CHECK_CHANGED } from "../render.ts";

/**
 * The files `carrick check --changed` judges: the scanner's own source
 * extensions (`SOURCE_EXTENSIONS` in `src/file_finder.rs`, carrick#904). An API
 * spec file is not counted: the hook cannot tell a spec the scan reads from any
 * other `.json` or `.yaml` without reading the repo, and a config file that
 * moves no contract is what the line used to be wrong about.
 */
const CHECKED_EXTENSIONS = new Set([".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"]);

/** `TEST_FILE_INFIXES` in `src/file_finder.rs`: a test or story file name ends its stem in one. */
const TEST_FILE_INFIXES = [".test", ".spec", ".stories"];

/** `TEST_DIR_NAMES` in `src/file_finder.rs` (plus `.storybook`): trees the scan does not read. */
const TEST_DIR_NAMES = new Set([
  "__tests__",
  "__mocks__",
  "__fixtures__",
  "tests",
  "test",
  "fixtures",
  "e2e",
  ".storybook",
]);

/** Whether `carrick check --changed` would judge this edited file: source, and not a test or story. */
export function isCheckedSource(file: string): boolean {
  const extension = path.extname(file).toLowerCase();
  if (!CHECKED_EXTENSIONS.has(extension)) return false;
  const stem = path.basename(file, extension).toLowerCase();
  if (TEST_FILE_INFIXES.some((infix) => stem.endsWith(infix))) return false;
  // Directory names above the file. The scanner reads them relative to the
  // scan root; the hook does not know it, so a checkout under a directory
  // named `test` hides its source here: the line is then missing, not wrong.
  const directories = path.dirname(file).split(/[\\/]/);
  return !directories.some((name) => TEST_DIR_NAMES.has(name.toLowerCase()));
}

/** The tools Claude Code edits a file with, each naming it as `input.file_path`. */
const EDIT_TOOLS = new Set(["Edit", "Write", "MultiEdit"]);

/** `carrick check …`, also as `npx carrick check`, `npx carrick@latest check`. */
const CHECK_COMMAND = /(^|[\s;&|(/])carrick(@\S+)?\s+check(\s|$)/;

/** The source edits since the last `carrick check`, and the newest of them. */
export type UncheckedEdits = {
  /** Distinct absolute paths edited after the last check, in first-edit order. */
  files: string[];
  /** Whether any `carrick check` ran earlier in the transcript. */
  checkedBefore: boolean;
  /** Identifies the newest such edit, so one edit set is named once. */
  lastEdit: string;
};

type ToolUse = { id?: unknown; name?: unknown; input?: unknown };

function toolUses(line: string): ToolUse[] {
  let entry: unknown;
  try {
    entry = JSON.parse(line);
  } catch {
    return [];
  }
  if (typeof entry !== "object" || entry === null) return [];
  const message = (entry as { message?: unknown }).message;
  if (typeof message !== "object" || message === null) return [];
  const content = (message as { content?: unknown }).content;
  if (!Array.isArray(content)) return [];
  return content.filter(
    (block): block is ToolUse =>
      typeof block === "object" && block !== null && (block as { type?: unknown }).type === "tool_use",
  );
}

function stringField(input: unknown, field: string): string | null {
  if (typeof input !== "object" || input === null) return null;
  const value = (input as Record<string, unknown>)[field];
  return typeof value === "string" ? value : null;
}

/**
 * The file an edit tool names, read the way `filesFromPayload` in
 * `post-edit.ts` reads a PostToolUse payload: `file_path`, `filePath`, or the
 * first of a MultiEdit's `edits`. Not imported from there because importing
 * that module runs the post-edit hook.
 */
function editedFile(input: unknown): string | null {
  const named = stringField(input, "file_path") ?? stringField(input, "filePath");
  if (named !== null) return named;
  const edits = typeof input === "object" && input !== null ? (input as { edits?: unknown }).edits : null;
  return Array.isArray(edits) ? stringField(edits[0], "file_path") : null;
}

/**
 * Walk a transcript's JSONL text and return the source edits made after its
 * last `carrick check`, or null when there are none.
 */
export function uncheckedEdits(transcript: string): UncheckedEdits | null {
  let files: string[] = [];
  let lastEdit = "";
  let checkedBefore = false;
  const lines = transcript.split("\n");
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index]!;
    // Most lines are text and tool results; skip them without a parse.
    if (!line.includes('"tool_use"')) continue;
    for (const use of toolUses(line)) {
      if (use.name === "Bash") {
        const command = stringField(use.input, "command");
        if (command !== null && CHECK_COMMAND.test(command)) {
          files = [];
          lastEdit = "";
          checkedBefore = true;
        }
        continue;
      }
      if (typeof use.name !== "string" || !EDIT_TOOLS.has(use.name)) continue;
      const file = editedFile(use.input);
      if (file === null || !isCheckedSource(file)) continue;
      if (!files.includes(file)) files.push(file);
      lastEdit = typeof use.id === "string" && use.id !== "" ? use.id : `line:${index}`;
    }
  }
  return files.length === 0 ? null : { files, checkedBefore, lastEdit };
}

/** Each git call is bounded: the Stop hook has to stay well under a second. */
const GIT_TIMEOUT_MS = 1500;

/** Where `carrick check --changed` compares against, tried in order (`git_state.rs`). */
const BASE_CANDIDATES = ["origin/HEAD", "origin/main", "origin/master", "main", "master"];

function git(cwd: string, args: string[]): string | null {
  const env: NodeJS.ProcessEnv = { ...process.env, GIT_OPTIONAL_LOCKS: "0" };
  for (const name of ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"]) delete env[name];
  const run = spawnSync("git", args, { cwd, env, encoding: "utf8", timeout: GIT_TIMEOUT_MS });
  return run.status === 0 && typeof run.stdout === "string" ? run.stdout : null;
}

/** The nearest directory above `file` that exists, and the part below it. */
function nearestExisting(file: string): { dir: string; rest: string } | null {
  let dir = path.dirname(file);
  while (!fs.existsSync(dir)) {
    const parent = path.dirname(dir);
    if (parent === dir) return null;
    dir = parent;
  }
  try {
    const real = fs.realpathSync(dir);
    return { dir: real, rest: path.relative(dir, file) };
  } catch {
    return null;
  }
}

/**
 * The files (repo-relative) that still differ from what `carrick check
 * --changed` compares against: the working tree against the merge base with
 * the default branch, which covers uncommitted and committed-not-merged edits,
 * plus untracked files. Null when git cannot answer.
 */
function stillChanged(toplevel: string): Set<string> | null {
  let base = "HEAD";
  for (const candidate of BASE_CANDIDATES) {
    const found = git(toplevel, ["merge-base", "HEAD", candidate]);
    if (found !== null && found.trim() !== "") {
      base = found.trim();
      break;
    }
  }
  const changed = git(toplevel, ["diff", "--name-only", "--no-renames", base]);
  const untracked = git(toplevel, ["ls-files", "--others", "--exclude-standard"]);
  if (changed === null || untracked === null) return null;
  return new Set(`${changed}${untracked}`.split("\n").filter((name) => name !== ""));
}

/**
 * Drop the edits whose file no longer differs: committed and merged, reverted,
 * or in a checkout that is gone. A file git cannot answer for (no checkout
 * above it, an error, a timeout) is kept.
 */
export function stillDiffering(files: string[]): string[] {
  const toplevels = new Map<string, string | null>();
  const changes = new Map<string, Set<string> | null>();
  return files.filter((file) => {
    const near = nearestExisting(file);
    if (near === null) return true;
    if (!toplevels.has(near.dir)) {
      const top = git(near.dir, ["rev-parse", "--show-toplevel"]);
      toplevels.set(near.dir, top === null ? null : top.trim());
    }
    const top = toplevels.get(near.dir);
    if (top === null || top === undefined || top === "") return true;
    if (!changes.has(top)) changes.set(top, stillChanged(top));
    const set = changes.get(top);
    if (set === null || set === undefined) return true;
    const relative = path.relative(top, path.join(near.dir, near.rest)).split(path.sep).join("/");
    return set.has(relative);
  });
}

/** The same, read from a transcript file. Null when it cannot be read. */
export function readUncheckedEdits(transcriptPath: unknown): UncheckedEdits | null {
  if (typeof transcriptPath !== "string" || transcriptPath === "") return null;
  let text: string;
  try {
    text = fs.readFileSync(transcriptPath, "utf8");
  } catch {
    return null;
  }
  const edits = uncheckedEdits(text);
  if (edits === null) return null;
  const files = stillDiffering(edits.files);
  return files.length === 0 ? null : { ...edits, files };
}

/** The line handed to the model. */
export function checkLine(edits: UncheckedEdits): string {
  const count = `${edits.files.length} file${edits.files.length === 1 ? "" : "s"}`;
  const since = edits.checkedBefore
    ? `This task edited ${count} since its last \`carrick check\`.`
    : `This task edited ${count} and has not run \`carrick check\`.`;
  return `Carrick: ${since} The per-edit re-check covers only the file each edit touched, so ${RUN_CHECK_CHANGED}`;
}
