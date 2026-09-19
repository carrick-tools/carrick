// Writing a file only where its bytes would change.
//
// Every writer in this folder goes through here, so re-running `carrick init`
// leaves an unchanged file's mtime alone: a settings file, a skill file and a
// proposal are all things a watcher, a build and a git status read.

import fs from "node:fs";
import path from "node:path";

export function writeIfChanged(target: string, body: string): "written" | "unchanged" {
  const existing = fs.existsSync(target) ? fs.readFileSync(target, "utf8") : null;
  if (existing === body) return "unchanged";
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, body);
  return "written";
}
