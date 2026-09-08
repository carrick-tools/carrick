// Which directory the workspace is, and how we decided.
//
// The root has to be the directory that holds `.carrick/`: the CLI reads its
// index from there, and every path it prints (`file`, `counterparts[].file`) is
// relative to it. Root the plugin one directory too deep and the CLI sees one
// service, reports nothing, and says so nowhere. The spike hit exactly that:
// Claude Code's rootUri follows the agent's shell cwd, so a `cd user-service`
// before the first edit moved it (runs/plugin-cd1, 2026-09-05).
//
// So the marker decides, not the client. Candidates are tried in order and the
// first one holding `.carrick/` wins; when the winner is not the directory the
// client sent, that is logged.

import fs from "node:fs";
import path from "node:path";

export const MARKER = ".carrick";

export type RootChoice = {
  root: string;
  /** Which candidate won: the client's folder, the env, an ancestor, or none. */
  source: "client" | "project_dir" | "ancestor" | "fallback";
  /** The workspace folder the client sent, when it sent one. */
  clientRoot: string | null;
  /** True when the client's folder is not the directory we are using. */
  differs: boolean;
  /** True when no candidate held `.carrick/`. */
  markerFound: boolean;
};

export function hasMarker(dir: string, exists = defaultExists): boolean {
  return exists(path.join(dir, MARKER));
}

function defaultExists(target: string): boolean {
  try {
    return fs.existsSync(target);
  } catch {
    return false;
  }
}

/** Every directory from `from` up to the filesystem root, nearest first. */
export function ancestors(from: string): string[] {
  const out: string[] = [];
  let current = path.resolve(from);
  for (;;) {
    out.push(current);
    const parent = path.dirname(current);
    if (parent === current) return out;
    current = parent;
  }
}

export type ResolveRootInput = {
  /** Workspace folder or rootUri path from the LSP client, when present. */
  clientRoot?: string | null;
  /** `CLAUDE_PROJECT_DIR`, when Claude Code set it. */
  projectDir?: string | null;
  /** The file being checked; its ancestors are the last resort. */
  filePath?: string | null;
  /** Injectable for tests. */
  exists?: (target: string) => boolean;
};

export function resolveRoot(input: ResolveRootInput): RootChoice {
  const exists = input.exists ?? defaultExists;
  const clientRoot = input.clientRoot ? path.resolve(input.clientRoot) : null;
  const projectDir = input.projectDir ? path.resolve(input.projectDir) : null;
  const fileDir = input.filePath ? path.dirname(path.resolve(input.filePath)) : null;

  const candidates: Array<{ dir: string; source: RootChoice["source"] }> = [];
  if (clientRoot) candidates.push({ dir: clientRoot, source: "client" });
  if (projectDir) candidates.push({ dir: projectDir, source: "project_dir" });
  if (fileDir) {
    for (const dir of ancestors(fileDir)) candidates.push({ dir, source: "ancestor" });
  }

  for (const candidate of candidates) {
    if (hasMarker(candidate.dir, exists)) {
      return {
        root: candidate.dir,
        source: candidate.source,
        clientRoot,
        differs: clientRoot !== null && clientRoot !== candidate.dir,
        markerFound: true,
      };
    }
  }

  const fallback = clientRoot ?? projectDir ?? fileDir ?? process.cwd();
  return {
    root: fallback,
    source: "fallback",
    clientRoot,
    differs: clientRoot !== null && clientRoot !== fallback,
    markerFound: false,
  };
}

/** One line for the log when the client's folder is not the root we chose. */
export function rootNote(choice: RootChoice): string | null {
  if (!choice.markerFound) {
    return `no ${MARKER}/ above the file; using ${choice.root} and expecting the CLI to say it is not indexed`;
  }
  if (!choice.differs) return null;
  return `client workspace folder ${choice.clientRoot} has no ${MARKER}/; using ${choice.root} (${choice.source})`;
}
