/**
 * Warm judge (carrick#2883): one judge workspace a resident sidecar keeps
 * between checks.
 *
 * A check on a kept workspace brings it to the new plan (writes the files
 * that changed, removes the files no longer planned), skips `pnpm install`
 * while the install key is unchanged, and runs `tsc` with its build info kept
 * in the workspace, so the compiler checks again only what the change reaches.
 *
 * Exactness: a changed key never installs in place. The first install writes
 * a lockfile, and an install over it would keep versions a cold run resolves
 * afresh; so a changed key replaces the workspace with a fresh one. Anything
 * that fails part way drops the workspace.
 *
 * Seam: node builtins + this bundle only.
 */

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { WriteGuard } from './guarded-fs.js';
import {
  entryDigest,
  installKeyOf,
  withDir,
  writeEntry,
  writeWorkspace,
  type AssembledWorkspace,
  type WorkspacePlan,
} from './check-workspace.js';

/** The compiler's build info, kept beside the workspace's manifests. */
export const JUDGE_BUILD_INFO = 'carrick-judge.tsbuildinfo';

/** In a kept workspace: the pid of the daemon that keeps it. */
export const JUDGE_OWNER_FILE = '.carrick-judge-owner';

/** The prefix every check workspace's directory name starts with (`writeWorkspace`). */
const CHECK_WORKSPACE_PREFIX = 'carrick-check-v2-';

/**
 * Remove the kept judge workspaces under `parent` whose owning daemon is dead:
 * one that aborts (out of heap) never discards its own. Only a directory that
 * carries the owner file is touched; returns how many were removed.
 */
export function removeOrphanedJudgeWorkspaces(parent: string = os.tmpdir()): number {
  let removed = 0;
  let entries: fs.Dirent[];
  try {
    entries = fs.readdirSync(parent, { withFileTypes: true });
  } catch {
    return 0;
  }
  for (const entry of entries) {
    if (!entry.isDirectory() || !entry.name.startsWith(CHECK_WORKSPACE_PREFIX)) continue;
    const dir = path.join(parent, entry.name);
    let owner: number;
    try {
      owner = Number(fs.readFileSync(path.join(dir, JUDGE_OWNER_FILE), 'utf8').trim());
    } catch {
      continue;
    }
    if (!Number.isInteger(owner) || owner <= 0 || alive(owner)) continue;
    try {
      WriteGuard.of({ dirs: [dir] }).remove(dir);
      removed += 1;
    } catch {
      /* best effort */
    }
  }
  return removed;
}

/** Bytes under `dir`, links counted as themselves. */
function sizeOnDisk(dir: string): number {
  let total = 0;
  const walk = (at: string): void => {
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(at, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const abs = path.join(at, entry.name);
      if (entry.isDirectory()) walk(abs);
      else {
        try {
          total += fs.lstatSync(abs).size;
        } catch {
          /* gone meanwhile */
        }
      }
    }
  };
  walk(dir);
  return total;
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    return (err as NodeJS.ErrnoException).code === 'EPERM';
  }
}

interface Held {
  ws: AssembledWorkspace;
  parent: string | undefined;
  installKey: string;
  /** Planned path -> digest of what is on disk now. */
  written: Map<string, string>;
  installed: boolean;
}

/** What `take` hands a check: the workspace, and whether it is installed. */
export interface TakenWorkspace {
  ws: AssembledWorkspace;
  installSkipped: boolean;
}

export class KeptJudge {
  private held: Held | null = null;
  private busy = false;
  /**
   * Checks that ran on the kept workspace, and those whose install was
   * skipped: counted here, never in a check's answer, which equals a cold
   * process's byte for byte.
   */
  readonly uses = { checks: 0, installs_skipped: 0 };
  /** Whether the last check that took the workspace skipped its install. */
  lastInstallSkipped: boolean | null = null;
  /** The kept workspace's size on disk (MB), measured when a check releases it; null when none is kept. */
  sizeMb: number | null = null;

  /** Whether a check holds the workspace now. */
  get inUse(): boolean {
    return this.busy;
  }

  /** The kept workspace's directory, or null when none is kept. */
  get workspaceDir(): string | null {
    return this.held?.ws.workspaceDir ?? null;
  }

  /**
   * The workspace for `plan`, or null while another check holds it (that
   * check's caller runs cold). Release it with `settle`.
   */
  take(plan: WorkspacePlan, parent: string | undefined): TakenWorkspace | null {
    if (this.busy) return null;
    this.busy = true;
    const installKey = installKeyOf(plan);
    const held = this.held;
    if (held && held.installed && held.installKey === installKey && held.parent === parent) {
      this.bringTo(held, plan);
      held.ws = withDir(plan, held.ws.workspaceDir, held.ws.guard);
      this.counted(true);
      return { ws: held.ws, installSkipped: true };
    }
    this.discard();
    this.busy = true;
    const ws = writeWorkspace(plan, parent);
    // Whoever finds this daemon dead removes the workspace (it never discards it after an abort).
    ws.guard.writeFile(path.join(ws.workspaceDir, JUDGE_OWNER_FILE), `${process.pid}\n`);
    const written = new Map<string, string>();
    for (const [rel, entry] of plan.files) written.set(rel, entryDigest(entry));
    this.held = { ws, parent, installKey, written, installed: false };
    this.counted(false);
    return { ws, installSkipped: false };
  }

  private counted(installSkipped: boolean): void {
    this.uses.checks += 1;
    if (installSkipped) this.uses.installs_skipped += 1;
    this.lastInstallSkipped = installSkipped;
  }

  /**
   * Release the workspace after a check. `installed` says the install
   * finished; `ok` false (a failed install, an abnormal compiler) drops it.
   */
  settle(ok: boolean, installed: boolean): void {
    this.busy = false;
    if (!this.held) return;
    if (!ok) {
      this.discard();
      return;
    }
    if (installed) this.held.installed = true;
    this.sizeMb = Math.round(sizeOnDisk(this.held.ws.workspaceDir) / 1048576);
  }

  /** Remove the kept workspace. */
  discard(): void {
    const held = this.held;
    this.held = null;
    this.busy = false;
    this.sizeMb = null;
    if (!held) return;
    try {
      held.ws.guard.remove(held.ws.workspaceDir);
    } catch {
      /* best effort */
    }
  }

  /** Write what changed, remove what is no longer planned. */
  private bringTo(held: Held, plan: WorkspacePlan): void {
    const { workspaceDir, guard } = held.ws;
    for (const [rel, entry] of plan.files) {
      const digest = entryDigest(entry);
      if (held.written.get(rel) === digest) continue;
      writeEntry(guard, workspaceDir, rel, entry);
      held.written.set(rel, digest);
    }
    for (const rel of [...held.written.keys()]) {
      if (plan.files.has(rel)) continue;
      removeFile(guard, path.join(workspaceDir, ...rel.split('/')));
      held.written.delete(rel);
    }
  }
}

function removeFile(guard: WriteGuard, file: string): void {
  if (fs.lstatSync(file, { throwIfNoEntry: false })) guard.remove(file);
}
