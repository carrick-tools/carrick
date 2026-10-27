/**
 * Keeping a held service exact after files change (carrick#2880).
 *
 * A daemon (`typed-server.ts`) holds a service's programs between requests.
 * Before a request reads them, and again after it, the files they were built
 * from are checked against the disk, whoever changed them (a hook, an
 * editor, a branch switch). What is found decides one of three things:
 *
 * - nothing changed: the held programs answer;
 * - only source text changed, in files whose imports and references are as
 *   they were: those files are read again into the held projects (refresh),
 *   so every other file keeps its parse and bind, and the program the next
 *   request builds holds the same files a cold process's would;
 * - anything else: the service is dropped and built again from its `init`,
 *   exactly as a cold process builds it. That is a config in the `extends`
 *   chain, a manifest, lockfile or install record, a file added to or removed
 *   from a config's file list, a file gone from disk, a change to what a
 *   file imports or references, more than {@link MAX_REFRESHED_FILES} changed
 *   files (a branch switch), and any change in a service whose projects are
 *   not all built from a tsconfig (default patterns, a tsconfig snapshot,
 *   Deno), whose file list is not re-read here.
 *
 * A file's stat (mtime, size, inode) is the first check; a changed stat is
 * confirmed against the text the program holds, so touching a file changes
 * nothing. Files the compiler serves from memory (its default library) are
 * not on disk and are skipped.
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import { ts, type Project } from 'ts-morph';

/** More changed files than this is a branch switch: rebuild instead. */
export const MAX_REFRESHED_FILES = 200;

/** Lockfiles and install records whose change means the install changed. */
const MANIFEST_NAMES = [
  'package.json',
  'pnpm-lock.yaml',
  'package-lock.json',
  'yarn.lock',
  'bun.lock',
  'bun.lockb',
  'deno.json',
  'deno.jsonc',
  'deno.lock',
  path.join('node_modules', '.modules.yaml'),
  path.join('node_modules', '.package-lock.json'),
  path.join('node_modules', '.yarn-state.yml'),
];

interface Stat {
  mtimeMs: number;
  size: number;
  ino: number;
}

/** What a sweep decided. */
export type Freshness =
  | { outcome: 'none' }
  | { outcome: 'refresh'; files: string[] }
  | { outcome: 'drop'; reason: string };

/** The parts of a held service the sweep reads. */
export interface HeldSources {
  /** Every project the service has built and holds. */
  projects(): Project[];
  /** Whether every held project is built from a tsconfig whose file list can be re-read. */
  refreshable(): boolean;
  /** Roots whose manifests count: the service root and the scan root. */
  roots(): string[];
}

function statOf(file: string): Stat | null {
  try {
    const stat = fs.statSync(file);
    return { mtimeMs: stat.mtimeMs, size: stat.size, ino: stat.ino };
  } catch {
    return null;
  }
}

function sameStat(a: Stat | undefined, b: Stat): boolean {
  return a !== undefined && a.mtimeMs === b.mtimeMs && a.size === b.size && a.ino === b.ino;
}

/** A file's text as the compiler reads it: without a byte-order mark. */
function diskText(file: string): string | null {
  try {
    const text = fs.readFileSync(file, 'utf8');
    return text.charCodeAt(0) === 0xfeff ? text.slice(1) : text;
  } catch {
    return null;
  }
}

/** What a file imports and references, the part of it that decides the program's file set. */
function structureOf(fileName: string, text: string): string {
  const info = ts.preProcessFile(text, true, true);
  return JSON.stringify([
    info.importedFiles.map((f) => f.fileName),
    info.referencedFiles.map((f) => f.fileName),
    info.typeReferenceDirectives.map((f) => [f.fileName, f.resolutionMode ?? null]),
    info.libReferenceDirectives.map((f) => f.fileName),
    info.ambientExternalModules ?? [],
    info.isLibFile,
    path.extname(fileName),
  ]);
}

/** The config file a project was built from, when it was built from one. */
function configOf(project: Project): string | undefined {
  const config = project.getCompilerOptions().configFilePath;
  return typeof config === 'string' ? config : undefined;
}

/** A config's file list and its `extends` chain, read from disk now. */
function readConfig(config: string): { fileNames: string[]; chain: string[] } | null {
  let failed = false;
  const parsed = ts.getParsedCommandLineOfConfigFile(config, undefined, {
    ...ts.sys,
    onUnRecoverableConfigFileDiagnostic: () => {
      failed = true;
    },
  });
  if (!parsed || failed) return null;
  const extended = (parsed.options.configFile as { extendedSourceFiles?: string[] } | undefined)?.extendedSourceFiles ?? [];
  return { fileNames: [...parsed.fileNames].sort(), chain: [config, ...extended] };
}

/**
 * The files one held service was built from, as they were when it last
 * answered, and the sweep that compares them with the disk.
 */
export class HeldFiles {
  /** Source files: stat when last seen equal to the program's text. */
  private readonly sources = new Map<string, Stat>();
  /** Configs and manifests: their text when the service was built. */
  private readonly settings = new Map<string, string | null>();
  /** Each tsconfig's file list when the service was built. */
  private readonly listings = new Map<string, string[]>();

  constructor(private readonly service: HeldSources) {}

  /**
   * Compare the disk with the held programs, record what is equal, and say
   * what must happen before the programs answer again.
   */
  sweep(): Freshness {
    const projects = this.service.projects();
    const setting = this.changedSetting(projects);
    if (setting) return { outcome: 'drop', reason: setting };

    const changed = new Map<string, { project: Project; text: string | null }[]>();
    for (const project of projects) {
      const program = project.getProgram().compilerObject;
      for (const file of program.getSourceFiles()) {
        if (program.isSourceFileDefaultLibrary(file)) continue;
        const stat = statOf(file.fileName);
        if (stat !== null && sameStat(this.sources.get(file.fileName), stat)) continue;
        const text = stat === null ? null : diskText(file.fileName);
        if (text === file.text) {
          if (stat) this.sources.set(file.fileName, stat);
          continue;
        }
        const seen = changed.get(file.fileName) ?? [];
        seen.push({ project, text });
        changed.set(file.fileName, seen);
      }
    }
    if (changed.size === 0) return { outcome: 'none' };
    if (!this.service.refreshable()) return { outcome: 'drop', reason: 'a service not built from a tsconfig changed' };
    if (changed.size > MAX_REFRESHED_FILES) return { outcome: 'drop', reason: `${changed.size} files changed` };
    for (const [file, seen] of changed) {
      const text = seen[0]!.text;
      if (text === null) return { outcome: 'drop', reason: `${file} is gone` };
      const held = seen[0]!.project.getSourceFile(file)?.getFullText();
      if (held === undefined || structureOf(file, held) !== structureOf(file, text)) {
        return { outcome: 'drop', reason: `${file} changed what it imports or references` };
      }
    }
    return { outcome: 'refresh', files: [...changed.keys()] };
  }

  /** Read `files` again into every held project that has them. */
  refresh(files: string[]): void {
    for (const project of this.service.projects()) {
      for (const file of files) project.getSourceFile(file)?.refreshFromFileSystemSync();
    }
    for (const file of files) this.sources.delete(file);
  }

  /**
   * Why the configs or manifests no longer match what was recorded, or
   * undefined when they do. A config or manifest seen for the first time (a
   * project built since the last sweep, a source file read from another
   * package) is recorded as it is now; a missing manifest is recorded as
   * missing, so one that appears later counts as a change.
   */
  private changedSetting(projects: Project[]): string | undefined {
    for (const [file, text] of this.settings) {
      if (diskText(file) !== text) return `${file} changed`;
    }
    for (const [config, listed] of this.listings) {
      const now = readConfig(config)?.fileNames ?? [];
      if (now.length !== listed.length || now.some((file, i) => file !== listed[i])) {
        return `${config} lists other files`;
      }
    }
    for (const project of projects) {
      const config = configOf(project);
      if (!config || this.listings.has(config)) continue;
      const read = readConfig(config);
      this.listings.set(config, read?.fileNames ?? []);
      for (const file of read?.chain ?? [config]) this.settings.set(file, diskText(file));
    }
    for (const file of this.manifests(projects)) {
      if (!this.settings.has(file)) this.settings.set(file, diskText(file));
    }
    return undefined;
  }

  /**
   * The manifests that decide how the service's imports resolve: under each
   * root, and the `package.json` nearest each source file outside an install.
   */
  private manifests(projects: Project[]): string[] {
    const files = new Set<string>();
    for (const root of this.service.roots()) {
      for (const name of MANIFEST_NAMES) files.add(path.join(root, name));
    }
    const nearest = new Map<string, string | null>();
    const packageJsonAbove = (dir: string): string | null => {
      if (nearest.has(dir)) return nearest.get(dir)!;
      const candidate = path.join(dir, 'package.json');
      const parent = path.dirname(dir);
      const found = fs.existsSync(candidate) ? candidate : parent === dir ? null : packageJsonAbove(parent);
      nearest.set(dir, found);
      return found;
    };
    for (const project of projects) {
      const program = project.getProgram().compilerObject;
      for (const file of program.getSourceFiles()) {
        if (program.isSourceFileDefaultLibrary(file) || file.fileName.includes('/node_modules/')) continue;
        const found = packageJsonAbove(path.dirname(file.fileName));
        if (found) files.add(found);
      }
    }
    return [...files].sort();
  }
}

/**
 * Answer with `run` until nothing changed while it ran: `freshen` brings the
 * held programs up to date before each answer, and `stable` says whether the
 * disk still matches them after it. An answer read while files changed is
 * dropped. Null when `attempts` answers were all dropped.
 */
export function answerWhileStable<T>(
  run: () => T,
  freshen: () => void,
  stable: () => boolean,
  attempts: number
): T | null {
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    freshen();
    const answer = run();
    if (stable()) return answer;
  }
  return null;
}
