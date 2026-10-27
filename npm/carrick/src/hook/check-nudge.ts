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

/** The same, read from a transcript file. Null when it cannot be read. */
export function readUncheckedEdits(transcriptPath: unknown): UncheckedEdits | null {
  if (typeof transcriptPath !== "string" || transcriptPath === "") return null;
  let text: string;
  try {
    text = fs.readFileSync(transcriptPath, "utf8");
  } catch {
    return null;
  }
  return uncheckedEdits(text);
}

/** The line handed to the model. */
export function checkLine(edits: UncheckedEdits): string {
  const count = `${edits.files.length} file${edits.files.length === 1 ? "" : "s"}`;
  const since = edits.checkedBefore
    ? `This task edited ${count} since its last \`carrick check\`.`
    : `This task edited ${count} and has not run \`carrick check\`.`;
  return `${since} The per-edit re-check covers only the file each edit touched, so ${RUN_CHECK_CHANGED}`;
}
