/**
 * An `infer` answer kept on disk and served again while nothing it was read
 * from has changed (carrick#2843).
 *
 * A scoped re-scan re-analyses a service when one of its imports changed,
 * even when nothing the service's own types are read from did. A generated
 * API client asks hundreds of `infer` requests of a program that a change
 * elsewhere does not touch, and answering them costs tens of seconds each
 * time. This keeps the last answer per service and serves it when the
 * program it was read in is the same program.
 *
 * What keys an answer, all of it compared before one is served:
 * - the sidecar itself: its compiled code, the TypeScript and ts-morph
 *   versions it runs and the Node version (`sidecarIdentity`);
 * - the `init` request the process was scoped with, less its id;
 * - the `infer` request, less its id: every item, the extraction config, the
 *   model symbols and the server claims;
 * - for each program the request is answered in: its root files in order,
 *   its compiler options, and every source file the compiler holds, by path
 *   and by the sha256 of its text (`programRecord`).
 *
 * The program is the compiler's whole view: a declaration it could read is a
 * source file in it, and a file that would now resolve in place of another
 * changes the list. To compare it, the lookup builds each program afresh from
 * its tsconfig, adds the roots the stored answer's program had, in order, and
 * compares the result. A program that differs is dropped again, so the
 * inference that follows builds it exactly as it would have with no cache.
 * A program that matches is the one the inference would have left behind, so
 * every later request in the process reads the program it would have read.
 *
 * Not kept: an answer whose unwidened reading ran out of its time budget (it
 * depends on the clock), an answer that read server-framework claims (they
 * are read in a program of their own, which the record does not cover), and
 * an answer of a process whose program a request before it had built (its
 * roots then depend on that request).
 *
 * On only where `CARRICK_INFER_CACHE_DIR` names a directory: local scans set
 * it, and a CI scan, which starts on a clean machine, does not.
 */

import { createHash } from 'node:crypto';
import * as fs from 'node:fs';
import { createRequire } from 'node:module';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import type { Project } from 'ts-morph';
import { WriteGuard } from './capture/guarded-fs.js';
import { listProgramFiles } from './program-files.js';
import type { InferredType } from './types.js';

/** The variable naming the directory answers are kept in. */
export const INFER_CACHE_ENV = 'CARRICK_INFER_CACHE_DIR';

/** Bumped when the entry's shape or what keys it changes. */
const ENTRY_VERSION = 1;

/** An entry older than this is deleted when another is written. */
const ENTRY_MAX_AGE_MS = 14 * 24 * 60 * 60 * 1000;

/** One program an answer was read in, as `programRecord` saw it. */
export interface ProgramRecord {
  key: string;
  /** The program's root files, in order. */
  roots: string[];
  /** Its compiler options, as JSON. */
  options: string;
  /** Every source file the compiler holds: path, module format, sha256 of its text. */
  sources: Array<[string, string, string]>;
}

/** The parts of an `infer` answer that are kept and served again. */
export interface KeptAnswer {
  status: 'success' | 'error';
  inferred_types?: InferredType[];
  errors?: string[];
}

interface Entry {
  version: number;
  identity: string;
  init: string;
  request: string;
  programs: ProgramRecord[];
  answer: KeptAnswer;
}

const sha256 = (text: string | Buffer): string => createHash('sha256').update(text).digest('hex');

/** The directory answers are kept in, or `undefined` when the cache is off. */
export function inferCacheDir(): string | undefined {
  const dir = process.env[INFER_CACHE_ENV];
  return dir && dir.trim() !== '' ? dir : undefined;
}

let identity: string | undefined;

/**
 * The sidecar that reads the answers: every compiled file of its own, the
 * compiler and ts-morph versions it loads, and the Node version.
 */
export function sidecarIdentity(): string {
  if (identity !== undefined) return identity;
  const here = path.dirname(fileURLToPath(import.meta.url));
  const files: string[] = [];
  const walk = (dir: string): void => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (entry.name.endsWith('.js')) files.push(full);
    }
  };
  walk(here);
  const hash = createHash('sha256');
  for (const file of files) {
    hash.update(path.relative(here, file)).update('\0').update(fs.readFileSync(file)).update('\0');
  }
  const require = createRequire(import.meta.url);
  for (const pkg of ['typescript', 'ts-morph']) {
    const manifest = JSON.parse(fs.readFileSync(require.resolve(`${pkg}/package.json`), 'utf8')) as {
      version?: string;
    };
    hash.update(`${pkg}@${manifest.version ?? ''}\0`);
  }
  hash.update(process.version);
  identity = hash.digest('hex');
  return identity;
}

/** A request's digest, less the id every request carries. */
export function requestDigest(request: object): string {
  const { request_id: _id, ...rest } = request as { request_id?: unknown };
  return sha256(JSON.stringify(rest));
}

/**
 * What `now` differs from `kept` in, first difference only, or `undefined`
 * when they are the same program: the same roots in the same order, the same
 * options, and the same source files with the same text.
 */
export function programDifference(kept: ProgramRecord, now: ProgramRecord): string | undefined {
  if (JSON.stringify(kept.roots) !== JSON.stringify(now.roots)) {
    const at = kept.roots.findIndex((file, i) => now.roots[i] !== file);
    const place = at === -1 ? now.roots.length - 1 : at;
    return `its root files, from root ${place}: ${kept.roots[place] ?? 'none'} kept, ${now.roots[place] ?? 'none'} now (${kept.roots.length} kept, ${now.roots.length} now)`;
  }
  if (kept.options !== now.options) return 'its compiler options';
  const before = new Map(kept.sources.map(([file, format, hash]) => [file, `${format}:${hash}`]));
  for (const [file, format, hash] of now.sources) {
    const was = before.get(file);
    if (was === undefined) return `${file}, which it now reads`;
    if (was !== `${format}:${hash}`) return `${file}, whose text changed`;
    before.delete(file);
  }
  const gone = before.keys().next();
  if (!gone.done) return `${gone.value}, which it no longer reads`;
  if (JSON.stringify(kept.sources.map(([f]) => f)) !== JSON.stringify(now.sources.map(([f]) => f))) {
    return 'the order of its source files';
  }
  return undefined;
}

/** What a kept answer is read against: the process and the request. */
export interface AnswerKey {
  /** The directory entries are kept in. */
  dir: string;
  /** The service root the process was scoped to: one entry per root. */
  repoRoot: string;
  /** `requestDigest` of the `init` request the process was scoped with. */
  init: string;
  /** `requestDigest` of the `infer` request. */
  request: string;
}

const entryPath = (key: AnswerKey): string => path.join(key.dir, `${sha256(key.repoRoot)}.json`);

/**
 * The kept entry for `key` when the sidecar, the scope and the request are
 * the ones it was kept for. The programs are not compared here: that needs
 * them built (`servedAnswer` in `index.ts`).
 */
export function readEntry(key: AnswerKey): { programs: ProgramRecord[]; answer: KeptAnswer } | undefined {
  let entry: Entry;
  try {
    entry = JSON.parse(fs.readFileSync(entryPath(key), 'utf8')) as Entry;
  } catch {
    return undefined;
  }
  if (
    entry.version !== ENTRY_VERSION ||
    entry.identity !== sidecarIdentity() ||
    entry.init !== key.init ||
    entry.request !== key.request ||
    !Array.isArray(entry.programs) ||
    !entry.answer
  ) {
    return undefined;
  }
  return { programs: entry.programs, answer: entry.answer };
}

/** Whether `answer` can be served again: it does not depend on the clock. */
export function keepable(answer: KeptAnswer): boolean {
  return !(answer.inferred_types ?? []).some((t) => t.unwidened_out_of_budget === true);
}

/**
 * Everything the compiler holds for `project` as it stands, under `key`: its
 * root files in order, its options, and each source file's path, module
 * format and the sha256 of its text.
 */
export function programRecord(key: string, project: Project): ProgramRecord {
  const program = project.getProgram().compilerObject;
  return {
    key,
    roots: listProgramFiles(project),
    options: JSON.stringify(program.getCompilerOptions()),
    sources: program
      .getSourceFiles()
      .map((file) => [file.fileName, String(file.impliedNodeFormat ?? ''), sha256(file.text)]),
  };
}

/**
 * The first source file of `kept` whose text on disk is not the text it was
 * read with, or `undefined` when every one on disk is the same. Read before any
 * program is built: a changed file, the usual reason an answer is stale, is
 * found for the cost of reading the files, and no program is built to find
 * it. The compiler reads a file without its byte-order mark, and so does
 * this.
 */
export function changedSource(kept: ProgramRecord): string | undefined {
  for (const [file, , hash] of kept.sources) {
    let text: string;
    try {
      text = fs.readFileSync(file, 'utf8');
    } catch {
      // Not on disk: ts-morph serves the default library from memory. A
      // file that is really gone is found when the program is built.
      continue;
    }
    if (text.charCodeAt(0) === 0xfeff) text = text.slice(1);
    if (sha256(text) !== hash) return `${file}, whose text changed`;
  }
  return undefined;
}

/**
 * Build `project`'s program again with nothing in it changed. ts-morph builds
 * a program only after a change, and roots it at every file the project has
 * loaded, the ones earlier builds reached among them: a program built again
 * after its first build has more roots than the first. Setting one file's
 * text to the text it has is the change.
 */
export function rebuildProgram(project: Project): void {
  const file = project.getSourceFiles()[0];
  if (!file) return;
  file.replaceWithText(file.getFullText());
  project.getProgram().compilerObject;
}

/**
 * Keep `answer` for `key`, read in `programs`. Written to a temporary file
 * and renamed, so a reader never sees half an entry. Entries not written to
 * for `ENTRY_MAX_AGE_MS` are deleted on the way.
 */
export function writeEntry(key: AnswerKey, programs: ProgramRecord[], answer: KeptAnswer): void {
  // The cache directory is the only place this writes, and never the
  // service it read (carrick#1748).
  const guard = WriteGuard.of({ dirs: [key.dir], protect: [key.repoRoot] });
  guard.mkdir(key.dir);
  const entry: Entry = {
    version: ENTRY_VERSION,
    identity: sidecarIdentity(),
    init: key.init,
    request: key.request,
    programs,
    answer,
  };
  const target = entryPath(key);
  const temporary = `${target}.${process.pid}.tmp`;
  guard.writeFile(temporary, JSON.stringify(entry));
  guard.rename(temporary, target);
  const now = Date.now();
  for (const name of fs.readdirSync(key.dir)) {
    const file = path.join(key.dir, name);
    try {
      if (now - fs.statSync(file).mtimeMs > ENTRY_MAX_AGE_MS) guard.remove(file);
    } catch {
      // Another process removed it first.
    }
  }
}
