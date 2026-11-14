// Which source files a shell command reads or writes (carrick hook post-bash).
//
// About nine in ten reads of a source file in an agent session go through the
// shell (`sed -n`, `cat`, `git show ref:path`), where the Read hook cannot
// fire. This reads the command text only: it runs nothing, and a path that
// does not exist on disk is not a target. Node built-ins only, because it runs
// before any other module loads and the hook's budget on a command that names
// no source file is a node start.

import fs from "node:fs";
import path from "node:path";
import { patchedFiles } from "./apply-patch.ts";

/** The files the index has rows for (the same set the Read hook checks). */
export const SOURCE_FILE = /\.(ts|tsx|mts|cts)$/;
/** A command that cannot name a source file is not parsed at all. */
export const MENTIONS_SOURCE = /\.(ts|tsx|mts|cts)\b/;
/**
 * The files the fact path (`facts/lines.ts`) reads: the above and JavaScript.
 * A JavaScript write gets fact lines only; the probe and the re-check flow
 * still run on `SOURCE_FILE` alone.
 */
export const FACT_FILE = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/;
export const MENTIONS_FACT_SOURCE = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)\b/;

/** One command of a line. `body` is the text of the heredoc it reads, when it has one. */
type Step = { words: string[]; writes: string[]; cwd: string; body: string; piped: boolean };

const READERS = new Set(["cat", "head", "tail", "nl", "less", "more", "bat", "tac"]);
const SEARCHERS = new Set(["grep", "egrep", "fgrep", "rg"]);
/** Search flags that take the next word as their value. */
const SEARCH_VALUE_FLAGS = new Set([
  "-e", "-f", "-g", "--regexp", "--file", "--glob",
  "-A", "-B", "-C", "-m", "-t", "-T", "--type", "--include", "--exclude", "--exclude-dir", "--max-count", "--max-depth",
]);
/** Of these, the ones that stand in for the pattern word. */
const PATTERN_FLAGS = new Set(["-e", "-f", "--regexp", "--file"]);

/** Split a command into steps on `;`, `&&`, `||`, `|` and newlines, quote-aware. A heredoc body is text, kept on its step. */
function steps(command: string, startCwd: string): Step[] {
  const out: Step[] = [];
  let cwd = startCwd;
  let words: string[] = [];
  let writes: string[] = [];
  let word = "";
  let inWord = false;
  let redirect = false;
  // The step being read is the right side of a `|`.
  let piped = false;
  // Heredocs opened on the current line: the step that reads each and its delimiter.
  let docs: Array<{ owner: number; delimiter: string; strip: boolean }> = [];
  const endWord = () => {
    if (!inWord) return;
    if (redirect) {
      writes.push(word);
      redirect = false;
    } else words.push(word);
    word = "";
    inWord = false;
  };
  const endStep = () => {
    endWord();
    redirect = false;
    if (words.length > 0 || writes.length > 0) {
      // `cd dir` moves every step after it.
      if (words[0] === "cd" && words[1] && words[1] !== "-") cwd = path.resolve(cwd, words[1]);
      out.push({ words, writes, cwd, body: "", piped });
    }
    piped = false;
    words = [];
    writes = [];
  };
  for (let i = 0; i < command.length; i += 1) {
    const ch = command[i]!;
    if (ch === "'" || ch === '"') {
      inWord = true;
      const close = command.indexOf(ch, i + 1);
      const end = close === -1 ? command.length : close;
      word += command.slice(i + 1, end);
      i = end;
    } else if (ch === "\\" && i + 1 < command.length) {
      inWord = true;
      word += command[i + 1];
      i += 1;
    } else if (ch === " " || ch === "\t") {
      endWord();
    } else if (ch === "\n") {
      endStep();
      // Each heredoc opened on the line just ended has its body next.
      for (const doc of docs) {
        let at = i + 1;
        const lines: string[] = [];
        while (at < command.length) {
          const end = command.indexOf("\n", at);
          const line = command.slice(at, end === -1 ? command.length : end);
          at = end === -1 ? command.length : end + 1;
          if ((doc.strip ? line.trimStart() : line) === doc.delimiter) break;
          lines.push(line);
        }
        const owner = out[doc.owner];
        if (owner) owner.body += `${lines.join("\n")}\n`;
        i = at - 1;
      }
      docs = [];
    } else if (ch === ";" || ch === "&" || ch === "|" || ch === "(" || ch === ")") {
      // A lone `|` hands this step's output to the next; `||` does not.
      const lone = ch === "|" && command[i + 1] !== "|" && command[i - 1] !== "|";
      endStep();
      piped = lone;
    } else if (ch === "<" && command[i + 1] === "<" && command[i + 2] === "<") {
      // A here-string: its word is an argument, not a file.
      endWord();
      i += 2;
    } else if (ch === "<" && command[i + 1] === "<") {
      // A heredoc: the rest of its line is still commands (`cat <<EOF > f.ts`),
      // its body is text.
      endWord();
      let j = i + 2;
      const strip = command[j] === "-";
      if (strip) j += 1;
      while (command[j] === " " || command[j] === "\t") j += 1;
      let delimiter = "";
      while (j < command.length && !/[\s;&|<>()]/.test(command[j]!)) {
        if (command[j] === "'" || command[j] === '"') {
          const close = command.indexOf(command[j]!, j + 1);
          const end = close === -1 ? command.length : close;
          delimiter += command.slice(j + 1, end);
          j = end + 1;
        } else {
          delimiter += command[j] === "\\" ? (command[j + 1] ?? "") : command[j];
          j += command[j] === "\\" ? 2 : 1;
        }
      }
      docs.push({ owner: out.length, delimiter, strip });
      i = j - 1;
    } else if (ch === ">") {
      // `2>file` is a write; a bare `2` before `>` is the descriptor, not a word.
      if (inWord && /^\d+$/.test(word)) {
        word = "";
        inWord = false;
      } else endWord();
      let j = i + 1;
      if (command[j] === ">" || command[j] === "|") j += 1;
      if (command[j] === "&") {
        // `2>&1`: a descriptor, not a file.
        j += 1;
        while (j < command.length && /[\w-]/.test(command[j]!)) j += 1;
        i = j - 1;
      } else {
        redirect = true;
        i = j - 1;
      }
    } else if (ch === "<") {
      endWord();
    } else {
      inWord = true;
      word += ch;
    }
  }
  endStep();
  return out;
}

/** The words that are not options, skipping the value of each flag in `valued`. */
function positionals(args: string[], valued: Set<string> = new Set()): { rest: string[]; flags: Set<string> } {
  const rest: string[] = [];
  const flags = new Set<string>();
  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i]!;
    if (arg.startsWith("-") && arg !== "-") {
      flags.add(arg);
      if (valued.has(arg)) i += 1;
    } else rest.push(arg);
  }
  return { rest, flags };
}

/** The nearest directory at or above `start` holding a `.git`, or null. */
function repoTop(start: string): string | null {
  let dir = start;
  for (;;) {
    if (fs.existsSync(path.join(dir, ".git"))) return dir;
    const parent = path.dirname(dir);
    if (parent === dir) return null;
    dir = parent;
  }
}

function isFile(file: string): boolean {
  try {
    return fs.statSync(file).isFile();
  } catch {
    return false;
  }
}

/** A script run by one of these names the files it writes in its own text. */
const SCRIPTERS = /^(python[\d.]*|node|nodejs|deno|bun|tsx|ruby|perl|php)$/;
/** A source path written into a script's text. */
const PATH_IN_TEXT = /[\w@~./-]*[\w-]\.(?:ts|tsx|mts|cts|js|jsx|mjs|cjs)\b/g;
/** Commands that move the tree in bulk: what they touch is not a write of the agent's. */
const BULK_GIT = new Set(["checkout", "switch", "rebase", "merge", "reset", "stash", "pull", "worktree", "restore", "cherry-pick", "revert"]);
const BULK_PACKAGE = new Set(["ci", "install", "i", "add", "update", "upgrade"]);

/**
 * Whether any command of `command` moves the tree in bulk (`git checkout`,
 * `npm ci` ...), which changes many files the agent did not write.
 */
export function movesTreeInBulk(command: string, cwd: string): boolean {
  for (const step of steps(command, cwd)) {
    let at = 0;
    while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(step.words[at] ?? "")) at += 1;
    const name = path.basename(step.words[at] ?? "");
    const args = step.words.slice(at + 1);
    if (name === "git") {
      let i = 0;
      while (i < args.length && args[i]!.startsWith("-")) i += args[i] === "-C" || args[i] === "-c" ? 2 : 1;
      if (BULK_GIT.has(args[i] ?? "")) return true;
    } else if (["npm", "pnpm", "yarn", "bun"].includes(name) && BULK_PACKAGE.has(args[0] ?? "")) return true;
  }
  return false;
}

/** One target named by a command: its path under each reading of the directory (`startReadings`). */
type Group = string[];

function sourceWords(words: string[], cwds: string[], kind: RegExp = SOURCE_FILE): Group[] {
  return words
    .filter((word) => kind.test(word) && !/[*?[{]/.test(word))
    .map((word) => [...new Set(cwds.map((cwd) => path.resolve(cwd, word)))]);
}

/** A file changed this much before the call started still counts as changed in its window. */
const WINDOW_SLACK_MS = 2000;

/** What the Bash hook knows about the call besides the command: how long it ran. */
export type CallInfo = { durationMs?: number; now?: number };

/**
 * The directories the command may have started in, given the payload's `cwd`.
 * The payload holds the directory AFTER the command when a `cd` persisted, and
 * the start directory when it ran in a subshell or did not persist. So: `cwd`
 * itself (reading a), and `cwd` with the command's net relative `cd` taken off
 * its end (reading b). A net move that is absolute or climbs out cannot be undone.
 */
function startReadings(command: string, cwd: string): string[] {
  const marker = path.join(path.parse(cwd).root, "carrick-start-marker");
  const all = steps(command, marker);
  const last = all[all.length - 1];
  const out = [cwd];
  if (!last || last.cwd === marker || !last.cwd.startsWith(`${marker}${path.sep}`)) return out;
  const net = path.relative(marker, last.cwd);
  if (cwd === net || !cwd.endsWith(`${path.sep}${net}`)) return out;
  const start = cwd.slice(0, cwd.length - net.length - 1) || path.parse(cwd).root;
  if (start !== cwd) out.push(start);
  return out;
}

/**
 * The paths of `group` that exist. A word that exists under both readings is
 * the one this command wrote when only one of them changed inside the call's
 * window; when that does not decide, both stay (a missed write is the worse error).
 */
function existing(group: Group, call: CallInfo): string[] {
  const found = group.filter(isFile);
  if (found.length < 2 || call.durationMs === undefined) return found;
  const now = call.now ?? Date.now();
  const from = now - call.durationMs - WINDOW_SLACK_MS;
  const changed = found.filter((file) => {
    try {
      const mtime = fs.statSync(file).mtimeMs;
      return mtime >= from && mtime <= now + WINDOW_SLACK_MS;
    } catch {
      return false;
    }
  });
  return changed.length === 1 ? changed : found;
}

/**
 * The source files `command` reads and the ones it writes, as absolute paths
 * that exist. A file the same command writes (`>`, `>>`, `sed -i`, `tee`) is
 * not in `reads`; the write is the other branch of the hook.
 */
export function bashTargets(
  command: string,
  cwd: string,
  call: CallInfo = {},
): { reads: string[]; writes: string[]; factWrites: string[] } {
  const reads: Group[] = [];
  const writes: Group[] = [];
  // Each reading of the start directory gives the same steps, so they zip.
  const readings = startReadings(command, cwd).map((start) => steps(command, start));
  for (const [n, first] of readings[0]!.entries()) {
    const step = first;
    const cwds = readings.map((one) => one[n]!.cwd);
    writes.push(...sourceWords(step.writes, cwds, FACT_FILE));
    // `apply_patch <<EOF` through the shell: the patch names its files.
    if (step.body.includes("*** Begin Patch")) {
      writes.push(...sourceWords(patchedFiles(step.body), cwds, FACT_FILE));
    }
    let at = 0;
    while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(step.words[at] ?? "")) at += 1;
    const name = path.basename(step.words[at] ?? "");
    const args = step.words.slice(at + 1);
    if (SCRIPTERS.test(name)) {
      // A script's text is the only place its target is named; whether it
      // wrote that file is decided by the file's mtime, not here.
      const text = `${step.body}\n${args.join("\n")}`;
      writes.push(...sourceWords(text.match(PATH_IN_TEXT) ?? [], cwds, FACT_FILE));
    } else if (name === "cp" || name === "mv" || name === "install") {
      const rest = positionals(args, new Set(["-t", "--target-directory"])).rest;
      // The destination is the last word; `-t dir` and a directory target name no file.
      writes.push(...sourceWords(rest.slice(-1), cwds, FACT_FILE));
    } else if (name === "tee") {
      writes.push(...sourceWords(positionals(args).rest, cwds, FACT_FILE));
    } else if (name === "sed") {
      const { rest, flags } = positionals(args, new Set(["-e", "-f", "--expression", "--file"]));
      const inPlace = [...flags].some((flag) => /^-[a-zA-Z]*i/.test(flag) || flag.startsWith("--in-place"));
      const scripted = ["-e", "-f", "--expression", "--file"].some((flag) => flags.has(flag));
      const operands = scripted ? rest : rest.slice(1);
      if (inPlace) writes.push(...sourceWords(operands, cwds, FACT_FILE));
      else reads.push(...sourceWords(operands, cwds));
    } else if (name === "awk") {
      reads.push(...sourceWords(positionals(args, new Set(["-f"])).rest.slice(1), cwds));
    } else if (READERS.has(name)) {
      reads.push(...sourceWords(positionals(args).rest, cwds));
    } else if (SEARCHERS.has(name)) {
      const { rest, flags } = positionals(args, SEARCH_VALUE_FLAGS);
      const patterned = [...flags].some((flag) => PATTERN_FLAGS.has(flag));
      reads.push(...sourceWords(patterned ? rest : rest.slice(1), cwds));
    } else if (name === "git") {
      // `git [-C dir] show <ref>:<path>`: the path is from the repo root and
      // the current tree answers.
      let dirs = cwds;
      let i = 0;
      while (i < args.length && args[i]!.startsWith("-")) {
        if (args[i] === "-C" && args[i + 1]) dirs = dirs.map((dir) => path.resolve(dir, args[i + 1]!));
        i += args[i] === "-C" || args[i] === "-c" ? 2 : 1;
      }
      if (args[i] !== "show") continue;
      const tops = [...new Set(dirs.map(repoTop).filter((top): top is string => top !== null))];
      if (tops.length === 0) continue;
      for (const arg of args.slice(i + 1)) {
        const split = arg.indexOf(":");
        const named = split > 0 ? arg.slice(split + 1) : "";
        if (named && SOURCE_FILE.test(named)) reads.push(tops.map((top) => path.join(top, named)));
      }
    }
  }
  const written = new Set(writes.flatMap((group) => existing(group, call)));
  return {
    reads: [...new Set(reads.flatMap((group) => existing(group, call)))].filter((file) => !written.has(file)),
    writes: [...written].filter((file) => SOURCE_FILE.test(file)),
    // JavaScript writes: fact lines only, never the probe (`FACT_FILE`).
    factWrites: [...written].filter((file) => !SOURCE_FILE.test(file)),
  };
}

/** Words a search pattern carries that are not the name searched for. */
const DECLARING = new Set(["function", "const", "let", "var", "export", "async", "default", "class", "return", "await", "type", "interface"]);

/**
 * The identifiers a grep or rg searches for, and the files it was pointed at.
 * A search piped from another command filters that command's output, not the
 * tree, so it names nothing. Each `|`-separated alternative must reduce to one
 * identifier of at least five characters (after dropping `function`, `const`
 * ...), so a phrase or a regex with structure names nothing.
 */
export function searchedNames(command: string, cwd: string, _call: CallInfo = {}): { names: string[]; operands: string[] } {
  const names: string[] = [];
  const operands: string[] = [];
  const readings = startReadings(command, cwd).map((start) => steps(command, start));
  for (const [n, step] of readings[0]!.entries()) {
    if (step.piped) continue;
    let at = 0;
    while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(step.words[at] ?? "")) at += 1;
    let name = path.basename(step.words[at] ?? "");
    let args = step.words.slice(at + 1);
    if (name === "git" && args.includes("grep")) {
      args = args.slice(args.indexOf("grep") + 1);
      name = "grep";
    }
    if (!SEARCHERS.has(name)) continue;
    const { rest, flags } = positionals(args, SEARCH_VALUE_FLAGS);
    // `-v` and `-L` ask for the lines or files that do NOT match.
    if ([...flags].some((flag) => /^-[a-zA-Z]*[vL]/.test(flag) && !flag.startsWith("--"))) continue;
    const patterns: string[] = [];
    for (let i = 0; i < args.length; i += 1) {
      if ((args[i] === "-e" || args[i] === "--regexp") && args[i + 1] !== undefined) patterns.push(args[i + 1]!);
    }
    const patterned = patterns.length > 0 || [...flags].some((flag) => PATTERN_FLAGS.has(flag));
    if (!patterned && rest[0] !== undefined) patterns.push(rest[0]);
    for (const operand of patterned ? rest : rest.slice(1)) {
      // An operand that exists under only one reading of the start directory is that one.
      const found = [...new Set(readings.map((one) => path.resolve(one[n]!.cwd, operand)))];
      const there = found.filter((file) => fs.existsSync(file));
      operands.push(...(there.length > 0 ? there : [found[0]!]));
    }
    for (const pattern of patterns) {
      for (const alt of pattern.split(/\\\||\|/)) {
        const tokens = alt
          .replace(/\\[bBsSwWdD<>]|[\\^$()*+?.[\]{}"'`=:;,<>]/g, " ")
          .split(/\s+/)
          .filter((token) => token !== "" && !DECLARING.has(token));
        const only = tokens.length === 1 ? tokens[0]! : "";
        if (only.length >= 5 && /^[A-Za-z_$][\w$]*$/.test(only) && !names.includes(only)) names.push(only);
      }
    }
  }
  return { names, operands };
}
