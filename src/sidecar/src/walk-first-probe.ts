/**
 * PROTOTYPE, never merged (one-engine step 0, S0-c). With
 * `CARRICK_PROBE_WALK_FIRST=kept|fresh`, an analysis process (one not scoped
 * to the workspace source, which only the position rows' process is) walks
 * every repo source file of its default program with `writePositionRows`
 * when that program is first built, before the request that built it is
 * answered. A box A/B then compares what the analysis emits against an
 * unpatched process, to measure what the walk's checker history moves.
 *
 * - `kept`: the walk's program and its checker stay; nothing is released.
 * - `fresh`: the parsed files stay, and both of the project's programs (the
 *   language service's, which the walk reads, and ts-morph's own) are
 *   rebuilt over the same source-file objects, each with a new checker.
 *   `languageService.cleanupSemanticCache()` drops the service's program;
 *   ts-morph's document registry hands the next program the source files it
 *   already holds (its release is a no-op), so nothing is parsed again.
 *   ts-morph's `Program` wrapper is reset by bumping the project version
 *   (`_context.resetProgram()`).
 */

import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import type { Project, ts } from 'ts-morph';
import { isDeclarationFileName } from './capture/index.js';
import { writePositionRows } from './position-rows.js';
import type { ProjectLoader } from './project-loader.js';

export type WalkFirstVariant = 'kept' | 'fresh';

/** What one probe walk did, for the test and the log line. */
export interface WalkFirstRecord {
  variant: WalkFirstVariant;
  files: number;
  rows: number;
  ms: number;
  /** The language service's program the walk read. */
  walked: ts.Program;
}

/** The variant `CARRICK_PROBE_WALK_FIRST` names, or `undefined`. */
export function walkFirstVariant(): WalkFirstVariant | undefined {
  const value = process.env.CARRICK_PROBE_WALK_FIRST;
  return value === 'kept' || value === 'fresh' ? value : undefined;
}

/**
 * The files the scanner's walk would send for this program: its source
 * files inside the repo root, outside `node_modules`, not declaration files,
 * and governed by the default project.
 */
export function walkedFilesOf(loader: ProjectLoader, project: Project): string[] {
  const root = loader.getRepoRoot() + path.sep;
  return project
    .getProgram()
    .compilerObject.getSourceFiles()
    .map((file) => path.resolve(file.fileName))
    .filter(
      (file) =>
        file.startsWith(root) &&
        !file.split(path.sep).includes('node_modules') &&
        !isDeclarationFileName(file) &&
        loader.projectKeyFor(file) === ''
    );
}

/** Walk `project` (the loader's default one, just built) as the variant says. */
export function walkFirst(loader: ProjectLoader, project: Project, variant: WalkFirstVariant): WalkFirstRecord {
  const started = performance.now();
  const files = walkedFilesOf(loader, project);
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-walk-first-'));
  let rows = 0;
  try {
    rows = writePositionRows({
      loader,
      files,
      scanRoot: loader.getScanRoot(),
      outPath: path.join(dir, 'rows.jsonl.gz'),
      service: 'walk-first-probe',
      keepPrograms: true,
    }).rows;
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
  const walked = project.getLanguageService().compilerObject.getProgram();
  if (!walked) throw new Error('the walk left no language service program');
  if (variant === 'fresh') {
    project.getLanguageService().compilerObject.cleanupSemanticCache();
    (project as unknown as { _context: { resetProgram(): void } })._context.resetProgram();
  }
  return { variant, files: files.length, rows, ms: Math.round(performance.now() - started), walked };
}
