// The files one `apply_patch` call leaves on disk (carrick#1335).
//
// Codex has no `Edit`/`Write` tool with a `file_path`. It edits through
// `apply_patch`, whose PostToolUse payload is
// `tool_input: { "command": "<the patch text>" }`
// (`codex-rs/core/src/tools/handlers/apply_patch.rs`), and one patch can add,
// update, delete and rename several files at once. So the recording half of
// the reuse nudge needs this: the patch text in, the paths worth re-checking
// out.
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
// Reference: `docs/reference/task-skills.md`, "The reuse nudge".

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
