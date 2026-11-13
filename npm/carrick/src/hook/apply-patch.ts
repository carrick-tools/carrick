// The files one `apply_patch` call leaves on disk (carrick#1335).
//
// Codex has no `Edit`/`Write` tool with a `file_path`. It edits through
// `apply_patch`, whose PostToolUse payload is
// `tool_input: { "command": "<the patch text>" }`
// (`codex-rs/core/src/tools/handlers/apply_patch.rs`), and one patch can add,
// update, delete and rename several files at once. So the post-edit hook
// needs this: the patch text in, the paths worth re-checking out.
//
// The grammar is four header lines, each at the start of its own line, inside
// a `*** Begin Patch` / `*** End Patch` envelope (`codex-rs/apply-patch`):
//
//     *** Add File: <path>
//     *** Update File: <path>
//     *** Delete File: <path>
//     *** Move to: <path>        (only ever after an Update File)
//
// Body lines carry a `+`, `-` or space prefix, so a line beginning `*** ` is a
// header and nothing else. `*** End of File` is a marker, not a path, and is
// ignored with everything else that is not one of the four.
//
// Two of the four are not simply "a file that changed":
//
// * `Delete File` names a path that is GONE once the patch applies. Re-checking
//   it asks the CLI about a file that is not there, so it is dropped here
//   rather than filtered later.
// * `Move to` renames the `Update File` above it. The file the index should be
//   compared against is the destination, so the destination replaces the source
//   rather than joining it.
//
// Reference: `docs/reference/task-skills.md`, "What the hooks deliver".

/** The header that opens a patch, used to tell a patch from any other string. */
const ENVELOPE = "*** Begin Patch";

const HEADERS = [
  { marker: "*** Add File: ", kind: "add" },
  { marker: "*** Update File: ", kind: "update" },
  { marker: "*** Delete File: ", kind: "delete" },
  { marker: "*** Move to: ", kind: "move" },
] as const;

type Header = { kind: (typeof HEADERS)[number]["kind"]; path: string };

function header(line: string): Header | null {
  for (const candidate of HEADERS) {
    if (line.startsWith(candidate.marker)) {
      const path = line.slice(candidate.marker.length).trim();
      if (path !== "") return { kind: candidate.kind, path };
    }
  }
  return null;
}

/** Whether a string is an `apply_patch` envelope at all. */
export function looksLikePatch(text: string): boolean {
  return text.includes(ENVELOPE);
}

/**
 * The paths a patch leaves on disk, in the order it names them.
 *
 * Relative to the cwd the tool ran in, exactly as the patch spells them, and
 * deduplicated: a caller turns them into workspace-relative paths because only
 * it knows the root.
 *
 * Anything that is not a patch gives an empty list, so a caller can hand this
 * whatever a `command` field held without asking first.
 */
export function patchedFiles(text: string): string[] {
  if (!looksLikePatch(text)) return [];
  const files: string[] = [];
  // Where the last `Update File` put its path, so a `Move to` under it replaces
  // that entry instead of adding a second one for a file that no longer exists.
  let lastUpdate: number | null = null;
  for (const line of text.split("\n")) {
    const found = header(line.trimEnd());
    if (found === null) continue;
    switch (found.kind) {
      case "delete":
        lastUpdate = null;
        break;
      case "move":
        if (lastUpdate === null) {
          files.push(found.path);
        } else {
          files[lastUpdate] = found.path;
        }
        lastUpdate = null;
        break;
      case "update":
        lastUpdate = files.push(found.path) - 1;
        break;
      case "add":
        files.push(found.path);
        lastUpdate = null;
        break;
    }
  }
  return [...new Set(files)];
}

/**
 * What a patch did to each file it leaves on disk: `added` for an
 * `*** Add File`, else its hunks, each the body lines (with their `+`, `-`
 * or space prefix) under one `@@`. A file the patch names twice, or both adds
 * and updates, is `null`: which lines are whose is not known.
 */
export function patchEdits(text: string): Map<string, { added: true } | { hunks: string[][] } | null> {
  const out = new Map<string, { added: true } | { hunks: string[][] } | null>();
  if (!looksLikePatch(text)) return out;
  let current: { path: string; hunks: string[][] } | null = null;
  let adding = false;
  const close = (): void => {
    if (current !== null) out.set(current.path, out.has(current.path) ? null : { hunks: current.hunks.filter((hunk) => hunk.length > 0) });
    current = null;
  };
  for (const raw of text.split("\n")) {
    const line = raw.replace(/\r$/, "");
    const found = header(line.trimEnd());
    if (found !== null) {
      if (found.kind === "move" && current !== null) {
        current.path = found.path;
        continue;
      }
      close();
      adding = false;
      if (found.kind === "add") {
        out.set(found.path, out.has(found.path) ? null : { added: true });
        adding = true;
      } else if (found.kind === "update") current = { path: found.path, hunks: [[]] };
      continue;
    }
    if (line.startsWith("*** End Patch")) {
      close();
      adding = false;
      continue;
    }
    if (adding || current === null) continue;
    if (line.startsWith("@@")) current.hunks.push([]);
    else if (line.startsWith("*** ")) continue;
    else if (/^[ +-]/.test(line) || line === "") current.hunks[current.hunks.length - 1]!.push(line === "" ? " " : line);
  }
  close();
  return out;
}

/**
 * Where `hunks` (from `patchEdits`) changed `text`, the file as it is now:
 * the 1-based lines they added, and the lines they removed lines just above.
 * Each hunk's new text (its context and `+` lines) must be found exactly once
 * in the file; otherwise null, and nothing is said about the file.
 * `removedText` is each removed line's text, in the order of `removed`.
 */
export function placeHunks(
  text: string,
  hunks: readonly string[][],
): { lines: number[]; removed: number[]; removedText: string[] } | null {
  const fileLines = text.split("\n");
  const lines: number[] = [];
  const removed: number[] = [];
  const removedText: string[] = [];
  for (const hunk of hunks) {
    const after = hunk.filter((line) => !line.startsWith("-")).map((line) => line.slice(1));
    if (after.length === 0) return null;
    let found = -1;
    for (let at = 0; at + after.length <= fileLines.length; at += 1) {
      if (after.every((line, k) => fileLines[at + k] === line)) {
        if (found >= 0) return null;
        found = at;
      }
    }
    if (found < 0) return null;
    let at = found + 1;
    for (const line of hunk) {
      if (line.startsWith("+")) lines.push(at++);
      else if (line.startsWith("-")) {
        removed.push(at);
        removedText.push(line.slice(1));
      } else at += 1;
    }
  }
  return { lines, removed, removedText };
}
