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

/** The files the index has rows for (the same set the Read hook checks). */
export const SOURCE_FILE = /\.(ts|tsx|mts|cts)$/;
/** A command that cannot name a source file is not parsed at all. */
export const MENTIONS_SOURCE = /\.(ts|tsx|mts|cts)\b/;

type Step = { words: string[]; writes: string[]; cwd: string };

const READERS = new Set(["cat", "head", "tail", "nl", "less", "more", "bat", "tac"]);
const SEARCHERS = new Set(["grep", "egrep", "fgrep", "rg"]);
/** Search flags that take the next word as their value. */
const SEARCH_VALUE_FLAGS = new Set(["-e", "-f", "-g", "--regexp", "--file", "--glob"]);
/** Of these, the ones that stand in for the pattern word. */
const PATTERN_FLAGS = new Set(["-e", "-f", "--regexp", "--file"]);

/** Split a command into steps on `;`, `&&`, `||`, `|` and newlines, quote-aware. Stops at a heredoc. */
function steps(command: string, startCwd: string): Step[] {
  const out: Step[] = [];
  let cwd = startCwd;
  let words: string[] = [];
  let writes: string[] = [];
  let word = "";
  let inWord = false;
  let redirect = false;
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
      out.push({ words, writes, cwd });
    }
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
    } else if (ch === ";" || ch === "&" || ch === "|" || ch === "\n" || ch === "(" || ch === ")") {
      endStep();
    } else if (ch === "<" && command[i + 1] === "<") {
      // A heredoc: its body is text, not commands, and nothing after it is read.
      endStep();
      return out;
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

function sourceWords(words: string[], cwd: string): string[] {
  return words
    .filter((word) => SOURCE_FILE.test(word) && !/[*?[{]/.test(word))
    .map((word) => path.resolve(cwd, word));
}

/**
 * The source files `command` reads and the ones it writes, as absolute paths
 * that exist. A file the same command writes (`>`, `>>`, `sed -i`, `tee`) is
 * not in `reads`; the write is the other branch of the hook.
 */
export function bashTargets(command: string, cwd: string): { reads: string[]; writes: string[] } {
  const reads: string[] = [];
  const writes: string[] = [];
  for (const step of steps(command, cwd)) {
    writes.push(...sourceWords(step.writes, step.cwd));
    let at = 0;
    while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(step.words[at] ?? "")) at += 1;
    const name = path.basename(step.words[at] ?? "");
    const args = step.words.slice(at + 1);
    if (name === "tee") {
      writes.push(...sourceWords(positionals(args).rest, step.cwd));
    } else if (name === "sed") {
      const { rest, flags } = positionals(args, new Set(["-e", "-f", "--expression", "--file"]));
      const inPlace = [...flags].some((flag) => /^-[a-zA-Z]*i/.test(flag) || flag.startsWith("--in-place"));
      const scripted = ["-e", "-f", "--expression", "--file"].some((flag) => flags.has(flag));
      const files = sourceWords(scripted ? rest : rest.slice(1), step.cwd);
      (inPlace ? writes : reads).push(...files);
    } else if (name === "awk") {
      reads.push(...sourceWords(positionals(args, new Set(["-f"])).rest.slice(1), step.cwd));
    } else if (READERS.has(name)) {
      reads.push(...sourceWords(positionals(args).rest, step.cwd));
    } else if (SEARCHERS.has(name)) {
      const { rest, flags } = positionals(args, SEARCH_VALUE_FLAGS);
      const patterned = [...flags].some((flag) => PATTERN_FLAGS.has(flag));
      reads.push(...sourceWords(patterned ? rest : rest.slice(1), step.cwd));
    } else if (name === "git") {
      // `git [-C dir] show <ref>:<path>`: the path is from the repo root and
      // the current tree answers.
      let dir = step.cwd;
      let i = 0;
      while (i < args.length && args[i]!.startsWith("-")) {
        if (args[i] === "-C" && args[i + 1]) dir = path.resolve(dir, args[i + 1]!);
        i += args[i] === "-C" || args[i] === "-c" ? 2 : 1;
      }
      if (args[i] !== "show") continue;
      const top = repoTop(dir);
      if (top === null) continue;
      for (const arg of args.slice(i + 1)) {
        const split = arg.indexOf(":");
        const named = split > 0 ? arg.slice(split + 1) : "";
        if (named && SOURCE_FILE.test(named)) reads.push(path.join(top, named));
      }
    }
  }
  const written = new Set(writes);
  return {
    reads: [...new Set(reads)].filter((file) => !written.has(file) && isFile(file)),
    writes: [...written].filter(isFile),
  };
}
