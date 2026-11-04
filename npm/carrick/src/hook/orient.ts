#!/usr/bin/env node
// SessionStart hook: one line telling the agent to read the project map first.
//
// The hosted scaffold printed this line from a shell script it wrote into each
// repo. It is the same line, from the same words, now a hook of its own so a
// test can switch it off alone. It reads no index and calls nothing: the slug it
// names is read back out of the task skills `carrick init` wrote, which carry
// the project every Carrick call in them is scoped to.
//
// Silent when the channel is off, when no task skill is installed, or when a
// committed settings file still runs the old pack's session-start script. Both
// Claude Code and Codex take this JSON shape from a SessionStart hook.

import fs from "node:fs";
import path from "node:path";

import { resolveChannel } from "../channel.ts";
import { createLogger } from "../log.ts";
import { scopeArgument, scopeOf, skillFile, SKILL_ROOTS, TASK_SKILLS, type SkillScope } from "../init/task-skills.ts";
import { candidateRoots, emit, oldPackWired, parsePayload, readStdin } from "./levers.ts";

const log = createLogger("carrick-orient");

/** The scope the first installed task skill under any of these folders was written with. */
export function installedScope(roots: string[]): SkillScope | null {
  for (const root of roots) {
    for (const skillRoot of SKILL_ROOTS) {
      for (const name of TASK_SKILLS) {
        try {
          return scopeOf(fs.readFileSync(path.join(root, skillFile(skillRoot, name)), "utf8"));
        } catch {
          // Not installed here; look at the next.
        }
      }
    }
  }
  return null;
}

/** The line, for one scope. */
export function orientLine(scope: SkillScope): string {
  const argument = scopeArgument(scope);
  return `Your working directory is on disk and holds the code this session is about; \`ls\` it first, then read the repo you are asked about. Then, before other work in this session, call mcp__carrick__get_project_map with ${argument} and service set to the repo or service you are working in. The map names the sibling services this project indexes, the contracts crossing into and out of this repo, and the drill-down tool for each section. The map is the view from the index; every path it names is a file in one of the repos in this project. Its header states the commit each repo was indexed at: \`git diff --name-only <that commit>\` in the checkout lists the files the index is silent about. Open the file at a \`file_location\` before you state what it contains.`;
}

async function main(): Promise<void> {
  if (resolveChannel({ hooksInstalled: true }).channel === "off") {
    log("CARRICK_CHANNEL=off; printing nothing");
    return;
  }
  const payload = parsePayload<{ cwd?: string }>(await readStdin());
  const roots = candidateRoots(payload.cwd);
  if (oldPackWired(roots, "session-start.sh")) {
    log("the old pack's session-start script is wired; leaving the line to it");
    return;
  }
  const scope = installedScope(roots);
  if (scope === null) {
    log("no task skill installed; nothing to name");
    return;
  }
  process.stdout.write(emit("SessionStart", orientLine(scope)));
}

await main();
process.exit(0);
