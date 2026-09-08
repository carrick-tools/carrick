// The hook entries `carrick init` writes into a project's `.claude/settings.json`.
//
// Merged entry by entry, never wholesale. Two things already write to that
// file: a user's own hooks, and the MCP hook pack the cloud's `scaffold` tool
// installs for the hosted index. Replacing the `hooks` key would silently
// uninstall either of them, so this only ever touches entries whose command is
// this CLI, and leaves the rest of the file — keys, order and formatting of
// what it does not own — as it found it.

export const HOOK_COMMAND_PREFIX = "carrick hook ";

export type HookEntry = { type: "command"; command: string; timeout?: number };
export type HookGroup = { matcher?: string; hooks: HookEntry[] };

/** What this package registers, and the only entries it will ever remove. */
export function carrickHooks(): Record<string, HookGroup[]> {
  return {
    // Attached to the tool result, so the verdicts arrive in the same turn as
    // the edit rather than the turn after it.
    PostToolUse: [
      {
        matcher: "Write|Edit|MultiEdit",
        hooks: [{ type: "command", command: "carrick hook post-edit", timeout: 15 }],
      },
    ],
    // No matcher: a resumed, cleared or compacted session has lost the map and
    // re-orients for one read of the index.
    SessionStart: [
      {
        hooks: [{ type: "command", command: "carrick hook session-start", timeout: 30 }],
      },
    ],
  };
}

function isOurs(entry: unknown): boolean {
  return (
    typeof entry === "object" &&
    entry !== null &&
    typeof (entry as HookEntry).command === "string" &&
    (entry as HookEntry).command.startsWith(HOOK_COMMAND_PREFIX)
  );
}

/** Strip our entries from one event's groups, keeping everyone else's. */
function withoutOurs(groups: unknown): HookGroup[] {
  if (!Array.isArray(groups)) return [];
  const kept: HookGroup[] = [];
  for (const group of groups) {
    if (typeof group !== "object" || group === null) {
      kept.push(group as HookGroup);
      continue;
    }
    const hooks = (group as HookGroup).hooks;
    if (!Array.isArray(hooks)) {
      kept.push(group as HookGroup);
      continue;
    }
    const others = hooks.filter((entry) => !isOurs(entry));
    // A group that held only our entry goes with it; one that held a mix keeps
    // the rest of its entries and its matcher.
    if (others.length > 0) kept.push({ ...(group as HookGroup), hooks: others });
  }
  return kept;
}

export type MergeResult = {
  /** The file body to write. */
  body: string;
  /** True when it differs from what was there. */
  changed: boolean;
};

/**
 * Merge this package's hooks into a settings file, or produce one.
 *
 * Throws when the existing file is not JSON: a settings file a user has hand
 * edited into invalid JSON is a thing to report, never a thing to overwrite.
 */
export function mergeCarrickHooks(existing: string | null): MergeResult {
  const base: Record<string, unknown> =
    existing == null || existing.trim() === ""
      ? {}
      : (JSON.parse(existing) as Record<string, unknown>);

  const hooksBefore = (base["hooks"] ?? {}) as Record<string, unknown>;
  const hooks: Record<string, unknown> = {};
  for (const [event, groups] of Object.entries(hooksBefore)) {
    hooks[event] = withoutOurs(groups);
  }
  for (const [event, groups] of Object.entries(carrickHooks())) {
    hooks[event] = [...((hooks[event] as HookGroup[]) ?? []), ...groups];
  }
  // An event that only ever held our entry, and no longer does, leaves no
  // empty array behind to puzzle over.
  for (const [event, groups] of Object.entries(hooks)) {
    if (Array.isArray(groups) && groups.length === 0) delete hooks[event];
  }

  const merged: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(base)) {
    merged[key] = key === "hooks" ? hooks : value;
  }
  if (!("hooks" in merged)) merged["hooks"] = hooks;

  const body = `${JSON.stringify(merged, null, 2)}\n`;
  return { body, changed: body !== existing };
}
