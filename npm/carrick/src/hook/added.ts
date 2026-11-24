// Which functions of an edited file the edit itself added (carrick#5121).
// The binary lists every function the working tree declares and the index does
// not; when the checkout is ahead of the index that includes functions the edit
// never touched. The reuse block speaks only for a function the edit added.

import fs from "node:fs";
import path from "node:path";
import { textAtCommit, untrackedFile } from "../facts/diff.ts";

/**
 * What the edit added, from the hook input. `whole`: the file was created by
 * the call. `lines`: the lines (of the file as it is now) the patch added, and
 * `before`: the text the edit replaced, or the file as it was before a `Write`,
 * so a declaration the edit only re-spelled is not read as new. Absent (the
 * caller has none): the patch is not known, and `isAdded` reads git.
 */
export type Added = { whole: true } | { lines: number[]; before: string[] };

/** The member name of `name` (`Class.member` -> `member`), escaped for a pattern. */
function bare(name: string): string {
  return (name.split(".").pop() ?? name).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Whether `text` declares a function, method or arrow-valued binding called
 * `name`. Loose on purpose: a doubtful match reads as "already there", which
 * keeps the block quiet.
 */
export function declares(text: string, name: string): boolean {
  const n = bare(name);
  const head = `\\b(?:function|class|const|let|var)\\s*\\*?\\s*${n}\\b`;
  const method = `(?:^|[\\s;{}])(?:(?:public|private|protected|static|async|readonly|override|abstract|get|set)\\s+)*\\*?\\s*${n}\\s*[(<]`;
  const bound = `\\b${n}\\s*[:=]\\s*(?:async\\s*)?(?:function\\b|\\(|[\\w$]+\\s*=>)`;
  return new RegExp(`${head}|${method}|${bound}`, "m").test(text);
}

/**
 * Whether the function `name`, declared at `line` of `file`, is one this edit
 * added. With the patch: its opening line is a line the patch added and the
 * text the edit replaced did not declare it. Without one: it is absent from
 * `HEAD` of the checkout (an untracked file has nothing there); where git
 * cannot say, it is not said to be new.
 */
export function isAdded(added: Added | undefined, file: string, name: string, line: number): boolean {
  if (added !== undefined) {
    if ("whole" in added) return true;
    return added.lines.includes(line) && !added.before.some((text) => declares(text, name));
  }
  if (untrackedFile(file)) return true;
  const head = textAtCommit(file, "HEAD");
  return head !== null && !declares(head, name);
}

/** The absolute path of the edited file of a check answer. */
export function fileOf(root: string, relative: string): string {
  return path.isAbsolute(relative) ? relative : path.join(root, relative);
}

/** The parts of the hook payload `addedFromPayload` reads. */
export type EditPayload = {
  tool_name?: string;
  tool_input?: { command?: unknown };
  tool_response?: { type?: unknown; originalFile?: unknown };
};

/**
 * What the edit added, per file (`added.ts`), for the reuse block: a file the
 * call created is whole; a placed patch gives its added lines and the text it
 * removed; a `Write` over an existing file is compared with `originalFile`.
 * Anything else (no patch, a hunk a formatter moved, a `Write` that does not
 * say what it replaced) is left out, and the block reads git for it.
 */
export async function addedFromPayload(payload: EditPayload, edited: string[], changes: ReadonlyMap<string, { lines?: number[] | null; removedText?: string[] | null }> | undefined): Promise<Map<string, Added>> {
  const out = new Map<string, Added>();
  if (edited.length !== 1 || typeof (payload.tool_input ?? {}).command === "string") {
    // A Codex patch names its files in the patch text; a placed hunk carries them below.
    for (const file of edited) {
      const change = changes?.get(file);
      if (change?.lines && change.removedText) out.set(file, { lines: change.lines, before: change.removedText });
    }
    return out;
  }
  const file = edited[0]!;
  const kind = payload.tool_response?.type;
  const original = payload.tool_response?.originalFile;
  if (kind === "create") {
    out.set(file, { whole: true });
  } else if (payload.tool_name === "Write" || kind === "update") {
    if (typeof original === "string") {
      try {
        const { regionsBetween } = await import("../held/place.ts");
        const now = fs.readFileSync(file, "utf8");
        const lines = new Set<number>();
        for (const region of regionsBetween(original, now)) {
          const first = now.slice(0, region.newStart).split("\n").length;
          const last = now.slice(0, Math.max(region.newStart, region.newEnd - 1)).split("\n").length;
          for (let at = first; at <= last; at += 1) lines.add(at);
        }
        out.set(file, { lines: [...lines], before: [original] });
      } catch {
        // Unreadable now: the block reads git.
      }
    }
  } else {
    const change = changes?.get(file);
    if (change?.lines && change.removedText) out.set(file, { lines: change.lines, before: change.removedText });
  }
  return out;
}

