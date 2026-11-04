// What the three activation hooks (orient, first-prompt, search-nudge) share.
//
// Each hook is its own command and its own settings entry so it can be dropped
// alone. They share only these small helpers, and none of them reads the index.

import fs from "node:fs";
import path from "node:path";

import { resolveRoot } from "../root.ts";

export async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString("utf8");
}

/** The JSON object a hook payload carries, or an empty one where it is not JSON. */
export function parsePayload<T extends object>(text: string): T {
  try {
    const parsed: unknown = JSON.parse(text || "{}");
    return typeof parsed === "object" && parsed !== null ? (parsed as T) : ({} as T);
  } catch {
    return {} as T;
  }
}

/** The one shape every hook event Carrick uses takes a model-visible line in. */
export function emit(event: "SessionStart" | "UserPromptSubmit" | "PreToolUse", context: string): string {
  return JSON.stringify({ hookSpecificOutput: { hookEventName: event, additionalContext: context } });
}

/** The folders this session could be rooted in, most specific first, without repeats. */
export function candidateRoots(payloadCwd?: string): string[] {
  const projectDir = process.env["CLAUDE_PROJECT_DIR"] || null;
  const resolved = resolveRoot({
    clientRoot: payloadCwd ?? process.cwd(),
    projectDir,
    filePath: payloadCwd ?? process.cwd(),
  }).root;
  return [...new Set([projectDir, payloadCwd, process.cwd(), resolved].filter((dir): dir is string => !!dir))];
}

/**
 * Whether a project's committed `.claude/settings.json` still runs one of the
 * hosted scaffold pack's scripts. `carrick init` swaps the pack out of a file it
 * writes, but never touches a committed one (repo-copies.ts), so the pack and
 * this hook would both speak. The pack wins there: stay silent.
 */
export function oldPackWired(roots: string[], script: "session-start.sh" | "turn-reminder.sh" | "search-gate.sh"): boolean {
  for (const root of roots) {
    try {
      const body = fs.readFileSync(path.join(root, ".claude", "settings.json"), "utf8");
      if (body.includes(`$CLAUDE_PROJECT_DIR/.claude/${script}`)) return true;
    } catch {
      // No settings file here, or one that cannot be read: nothing wired.
    }
  }
  return false;
}
