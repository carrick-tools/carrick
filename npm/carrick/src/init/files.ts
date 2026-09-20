// Writing a file only where its bytes would change.
//
// Every writer in this folder goes through here, so re-running `carrick init`
// leaves an unchanged file's mtime alone: a settings file, a skill file and a
// proposal are all things a watcher, a build and a git status read.

import fs from "node:fs";
import path from "node:path";

/**
 * The same text with one spelling of a line ending.
 *
 * What this package renders always ends its lines with `\n`. What comes back
 * off disk does not: a checkout with `core.autocrlf` rewrites every file it
 * tracks on the way out, so the eight skill files and both settings files
 * differ from the rendered body in every line and nothing else (carrick#1331).
 * Compared as raw bytes, that is a file to rewrite on every single
 * `carrick init` — and rewriting it puts the LF copy back, which git then
 * shows as a modified file in a repository somebody is working in.
 *
 * Comparison only. Nothing here converts what is written: this package writes
 * LF, and a file already on disk in CRLF is left in CRLF.
 */
export function sameText(existing: string | null, body: string): boolean {
  if (existing === null) return false;
  return existing.replace(/\r\n/g, "\n") === body.replace(/\r\n/g, "\n");
}

export function writeIfChanged(target: string, body: string): "written" | "unchanged" {
  const existing = fs.existsSync(target) ? fs.readFileSync(target, "utf8") : null;
  if (sameText(existing, body)) return "unchanged";
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, body);
  return "written";
}
