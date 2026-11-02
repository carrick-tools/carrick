// Whether a shell command, as the agent typed it, runs a text search.
//
// A TypeScript port of the awk the hosted scaffold's search gate carried: it
// reads the command each shell segment runs, so `echo 'grep x'` and a heredoc
// that holds the word `grep` are not searches, while `cd a && rg foo src` and
// `git grep foo` are. Kept in its own module because the hook that uses it runs
// on import.

const SEARCH = new Set(["grep", "egrep", "fgrep", "rg", "ag", "ack", "find", "fd"]);
const SKIP = new Set(["sudo", "time", "command", "nohup", "xargs", "if", "then", "else", "elif", "do", "while", "until", "for", "!"]);
const SEGMENT_SEPARATORS = /[|&;(){}`]+/;
const ASSIGNMENT = /^[A-Za-z_][A-Za-z0-9_]*=/;

/** True where any segment of the (quote- and heredoc-stripped) text runs a search command. */
function scan(text: string): boolean {
  for (const segment of text.split(SEGMENT_SEPARATORS)) {
    const words = segment.split(/\s+/).filter((word) => word !== "");
    let index = 0;
    while (index < words.length && (ASSIGNMENT.test(words[index]!) || SKIP.has(words[index]!))) index += 1;
    if (index >= words.length) continue;
    const command = words[index]!.replace(/^.*\//, "");
    if (command === "git") {
      for (let next = index + 1; next < words.length && next <= index + 3; next += 1) {
        if (words[next] === "grep") return true;
      }
      continue;
    }
    if (SEARCH.has(command)) return true;
  }
  return false;
}

/** The accepted search commands, quoted text and heredoc bodies ignored. */
export function searchCommandIn(command: string): boolean {
  let inSingle = false;
  let inDouble = false;
  let heredoc = "";
  let heredocStrip = false;
  let pending = "";
  for (const line of command.split("\n")) {
    if (heredoc !== "") {
      const body = heredocStrip ? line.replace(/^[ \t]+/, "") : line;
      if (body === heredoc) heredoc = "";
      continue;
    }
    let out = "";
    let i = 0;
    const n = line.length;
    while (i < n) {
      const c = line[i]!;
      if (inSingle) {
        if (c === "'") inSingle = false;
        i += 1;
        continue;
      }
      if (inDouble) {
        if (c === "\\") {
          i += 2;
          continue;
        }
        if (c === '"') inDouble = false;
        i += 1;
        continue;
      }
      if (c === "\\") {
        i += 2;
        out += "Q";
        continue;
      }
      if (c === "'") {
        inSingle = true;
        out += "Q";
        i += 1;
        continue;
      }
      if (c === '"') {
        inDouble = true;
        out += "Q";
        i += 1;
        continue;
      }
      if (c === "<" && line[i + 1] === "<") {
        let j = i + 2;
        if (line[j] === "<") {
          // A here-string: no body follows.
          i = j + 1;
          out += " ";
          continue;
        }
        heredocStrip = false;
        if (line[j] === "-") {
          heredocStrip = true;
          j += 1;
        }
        while (line[j] === " ") j += 1;
        let delimiter = "";
        while (j < n) {
          const ch = line[j]!;
          if (ch === " " || ch === ";" || ch === "|" || ch === "&" || ch === "<" || ch === ">") break;
          if (ch !== "'" && ch !== '"' && ch !== "\\") delimiter += ch;
          j += 1;
        }
        pending = delimiter;
        i = j;
        out += " ";
        continue;
      }
      out += c;
      i += 1;
    }
    if (scan(out)) return true;
    if (pending !== "") {
      heredoc = pending;
      pending = "";
    }
  }
  return false;
}
