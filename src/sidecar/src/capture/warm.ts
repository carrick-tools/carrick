/**
 * Warm capture: what a resident sidecar keeps of one service's capture to
 * answer its next capture faster, with the same bytes (carrick#2882).
 *
 * A cold capture builds three programs (the anchors', the surface emit's and
 * the stub self-check's), emits every declaration and checks every stub file.
 * A held capture keeps three things across requests, each one reused only
 * where an edit cannot change what it produced:
 *
 * - **Parsed files** (`SourceFileCache`). Every program a hold builds reads
 *   its files through one cache: a file whose size and times have not changed
 *   since it was read is handed back as the same `SourceFile`, so only edited
 *   files are parsed again. A file is keyed by what decides its parse (its
 *   name, language version, module format, module detection and JSX mode),
 *   never by the program, so two programs may share it as an editor does.
 * - **Emitted declarations** (`EmitHold`). Each source file's declaration
 *   output, by the file. The next emit writes again only the surface entry,
 *   the files whose text changed, the files an edit can reach (`EmitReach`,
 *   the server's import graph), and files new to the program; every other
 *   file's output is the one kept. The compiler orders types by their content
 *   (`stableTypeOrdering`, carrick#2019), so a declaration printed alone reads
 *   as it does printed among all the others.
 * - **Stub diagnostics** (`SelfCheckHold`). Each stub file's own syntactic
 *   and semantic diagnostics, by its path in the stub and its text. The
 *   self-check reads only diagnostics a file earns from its own text, the
 *   files the stub holds and the global scope: a module it cannot find, and a
 *   name or namespace it cannot find. So a kept list answers while the stub
 *   holds the same files, no changed stub file declares anything global, and
 *   nothing the stub reads outside itself changed. Otherwise every stub file
 *   is checked again.
 *
 * Nothing here is used by a cold capture (stdio): `captureStub` without a
 * hold builds and checks as it always has.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';

/** What decides how a file parses, beside its name. */
function parseKey(fileName: string, version: ts.ScriptTarget | ts.CreateSourceFileOptions, options: ts.CompilerOptions): string {
  const settings =
    typeof version === 'object'
      ? [version.languageVersion, version.impliedNodeFormat ?? null, version.jsDocParsingMode ?? null]
      : [version, null, null];
  return JSON.stringify([fileName, ...settings, options.moduleDetection ?? null, options.jsx ?? null]);
}

/** A file's size and times: unchanged while none of them moves. */
function stamp(fileName: string): string | undefined {
  const stat = fs.statSync(fileName, { throwIfNoEntry: false });
  return stat ? `${stat.size}:${stat.mtimeMs}:${stat.ctimeMs}:${stat.ino}` : undefined;
}

/** Parsed files, kept across the programs of one held capture. */
export class SourceFileCache {
  private readonly files = new Map<string, { stamp: string; sourceFile: ts.SourceFile }>();
  /** Files parsed again since the cache was made, and handed back unparsed. */
  readonly counts = { parsed: 0, kept: 0 };

  /** `host`, reading its source files through this cache. Changes `host` in place. */
  wrap(host: ts.CompilerHost, options: ts.CompilerOptions): ts.CompilerHost {
    const read = host.getSourceFile.bind(host);
    host.getSourceFile = (fileName, version, onError, fresh) => {
      if (fresh) return read(fileName, version, onError, fresh);
      const key = parseKey(fileName, version, options);
      const now = stamp(fileName);
      const held = this.files.get(key);
      if (held && now !== undefined && held.stamp === now) {
        this.counts.kept += 1;
        return held.sourceFile;
      }
      const sourceFile = read(fileName, version, onError, fresh);
      if (!sourceFile || now === undefined) {
        this.files.delete(key);
        return sourceFile;
      }
      // Touched but not changed: the parse already held stands.
      if (held && held.sourceFile.text === sourceFile.text) {
        this.files.set(key, { stamp: now, sourceFile: held.sourceFile });
        this.counts.kept += 1;
        return held.sourceFile;
      }
      this.files.set(key, { stamp: now, sourceFile });
      this.counts.parsed += 1;
      return sourceFile;
    };
    return host;
  }
}

/** One source file's declaration output, as one emit of it wrote it. */
export interface EmittedOf {
  /** The text it was emitted from. */
  sourceFile: ts.SourceFile;
  /** Each file written, by its path from the staging directory, in order. */
  outputs: Array<{ rel: string; text: string; source: string | undefined }>;
  /** The emit's diagnostics for this file. */
  diagnostics: ts.Diagnostic[];
  /** Its declaration emit was skipped. */
  skipped: boolean;
}

/** A file an edit changed: its text before and after. */
export interface ChangedText {
  file: string;
  before: string;
  after: string;
}

/**
 * The files whose declarations an edit can change, beside the changed files
 * themselves: read from `before`, the program the kept output was emitted
 * from, and carried to `after`. `'all'` when it cannot say.
 */
export type EmitReach = (before: ts.Program, after: ts.Program, changes: ChangedText[]) => ReadonlySet<string> | 'all';

/** The last surface emit of a held capture. */
export interface EmitHold {
  program: ts.Program;
  /** The entry file of that emit: written again by every emit. */
  entryPath: string;
  /** By source file name. */
  byFile: Map<string, EmittedOf>;
}

/** One stub file's own diagnostics, with no position tied to a stub directory. */
interface KeptDiagnostic {
  start: number | undefined;
  length: number | undefined;
  code: number;
  category: ts.DiagnosticCategory;
  messageText: string | ts.DiagnosticMessageChain;
  reportsUnnecessary?: object;
  reportsDeprecated?: object;
}

/** The last self-check of a held capture. */
export interface SelfCheckHold {
  /** What the stub's files resolve through: its file list, and how it reads packages. */
  context: string;
  /**
   * By path from the stub directory: the texts checked and their diagnostics,
   * newest first, at most two. A file the self-check repairs is checked as
   * emitted and as repaired in every capture, so both are kept.
   */
  byFile: Map<string, Array<{ text: string; diagnostics: KeptDiagnostic[] }>>;
  /** Every file the self-check program read outside the stub, as the object it read. */
  outside: Map<string, ts.SourceFile>;
}

/** What a held capture reused, counted over its life. */
export interface CaptureHoldCounts {
  captures: number;
  /** Declaration outputs written again, and kept. */
  emitted: number;
  emit_kept: number;
  /** Stub files checked again, and kept. */
  checked: number;
  check_kept: number;
  /** Source files parsed again, and handed back unparsed. */
  parsed: number;
  parse_kept: number;
}

/**
 * Everything a resident sidecar keeps of one service's capture. Made by the
 * server, one per service, and passed to each `captureStub` of that service.
 */
export class CaptureHold {
  readonly sources = new SourceFileCache();
  emit: EmitHold | undefined;
  selfCheck: SelfCheckHold | undefined;
  readonly counts = { captures: 0, emitted: 0, emit_kept: 0, checked: 0, check_kept: 0 };
  lastUsed = 0;

  constructor(readonly reach?: EmitReach) {}

  /** Release every program and parse it keeps. */
  release(): void {
    this.emit = undefined;
    this.selfCheck = undefined;
  }

  report(): CaptureHoldCounts {
    return { ...this.counts, parsed: this.sources.counts.parsed, parse_kept: this.sources.counts.kept };
  }
}

/**
 * The declaration emit of `program`, from what `hold` kept where it still
 * answers. Writes `hold.emit` for the next one. Returns what one
 * `program.emit()` over the whole program would have: each output in program
 * order, the diagnostics sorted, and whether any file's emit was skipped.
 */
export function emitHeld(
  hold: CaptureHold,
  program: ts.Program,
  entryPath: string,
  staging: string
): { outputs: Array<{ fileName: string; text: string; source: string | undefined }>; diagnostics: ts.Diagnostic[]; skipped: boolean } {
  const kept = hold.emit;
  const again = writtenAgain(hold, kept, program, entryPath);
  const byFile = new Map<string, EmittedOf>();
  if (again === 'all') {
    // One emit of the whole program, as a cold capture runs it, split by file.
    const written = new Map<string, EmittedOf['outputs']>();
    const result = program.emit(
      undefined,
      (fileName, text, _bom, _error, sources) => {
        const source = sources?.[0]?.fileName;
        const key = source ?? '';
        written.set(key, [...(written.get(key) ?? []), { rel: path.relative(staging, fileName), text, source }]);
      },
      undefined,
      /* emitOnlyDtsFiles */ true
    );
    const diagnosed = new Map<string, ts.Diagnostic[]>();
    for (const diagnostic of result.diagnostics) {
      const key = diagnostic.file?.fileName ?? '';
      diagnosed.set(key, [...(diagnosed.get(key) ?? []), diagnostic]);
    }
    for (const sourceFile of program.getSourceFiles()) {
      const own = diagnosed.get(sourceFile.fileName) ?? [];
      byFile.set(sourceFile.fileName, {
        sourceFile,
        outputs: written.get(sourceFile.fileName) ?? [],
        diagnostics: own,
        // A file's declaration emit is skipped exactly when it has a declaration diagnostic.
        skipped: own.length > 0,
      });
      if (written.has(sourceFile.fileName)) hold.counts.emitted += 1;
    }
    hold.emit = { program, entryPath, byFile };
    const loose = [...written.keys()].filter((key) => !byFile.has(key));
    if (loose.length > 0 || diagnosed.has('')) {
      // Output or a diagnostic tied to no file of the program: nothing to keep it by.
      hold.emit = undefined;
    }
    return {
      outputs: [...written.values()].flat().map((o) => ({ fileName: path.join(staging, o.rel), text: o.text, source: o.source })),
      diagnostics: [...result.diagnostics],
      skipped: result.emitSkipped,
    };
  }
  const outputs: Array<{ fileName: string; text: string; source: string | undefined }> = [];
  const diagnostics: ts.Diagnostic[] = [];
  let skipped = false;
  for (const sourceFile of program.getSourceFiles()) {
    let entry = again.has(sourceFile.fileName) ? undefined : kept?.byFile.get(sourceFile.fileName);
    if (entry) {
      if (entry.outputs.length > 0) hold.counts.emit_kept += 1;
    } else {
      const written: EmittedOf['outputs'] = [];
      const result = program.emit(
        sourceFile,
        (fileName, text, _bom, _error, sources) =>
          written.push({ rel: path.relative(staging, fileName), text, source: sources?.[0]?.fileName }),
        undefined,
        /* emitOnlyDtsFiles */ true
      );
      entry = { sourceFile, outputs: written, diagnostics: [...result.diagnostics], skipped: result.emitSkipped };
      if (written.length > 0) hold.counts.emitted += 1;
    }
    byFile.set(sourceFile.fileName, entry);
    for (const output of entry.outputs) {
      outputs.push({ fileName: path.join(staging, output.rel), text: output.text, source: output.source });
    }
    diagnostics.push(...entry.diagnostics);
    skipped ||= entry.skipped;
  }
  hold.emit = { program, entryPath, byFile };
  return { outputs, diagnostics: [...ts.sortAndDeduplicateDiagnostics(diagnostics)], skipped };
}

/** The source files whose kept output does not answer for `program`, or `'all'`. */
function writtenAgain(
  hold: CaptureHold,
  kept: EmitHold | undefined,
  program: ts.Program,
  entryPath: string
): ReadonlySet<string> | 'all' {
  if (!kept || !hold.reach) return 'all';
  const own = (p: ts.Program, entry: string): string[] =>
    p.getSourceFiles().map((sourceFile) => sourceFile.fileName).filter((fileName) => fileName !== entry);
  const before = own(kept.program, kept.entryPath);
  const after = own(program, entryPath);
  // A file in the program or out of it moves what an import resolves to.
  if (before.length !== after.length || before.some((fileName, i) => fileName !== after[i])) return 'all';
  const changes: ChangedText[] = [];
  for (const fileName of after) {
    const was = kept.byFile.get(fileName)?.sourceFile;
    const now = program.getSourceFile(fileName)!;
    if (!was) return 'all';
    if (was !== now && was.text !== now.text) changes.push({ file: fileName, before: was.text, after: now.text });
  }
  const again = new Set<string>([entryPath, ...changes.map((change) => change.file)]);
  if (changes.length === 0) return again;
  const reached = hold.reach(kept.program, program, changes);
  if (reached === 'all') return 'all';
  for (const fileName of reached) again.add(fileName);
  return again;
}

/** A diagnostic as a stub file keeps it, free of the stub directory it was read in. */
function keep(diagnostic: ts.Diagnostic): KeptDiagnostic {
  return {
    start: diagnostic.start,
    length: diagnostic.length,
    code: diagnostic.code,
    category: diagnostic.category,
    messageText: diagnostic.messageText,
    reportsUnnecessary: diagnostic.reportsUnnecessary,
    reportsDeprecated: diagnostic.reportsDeprecated,
  };
}

/** A kept diagnostic, placed in `sourceFile`. */
function restore(kept: KeptDiagnostic, sourceFile: ts.SourceFile): ts.Diagnostic {
  return { ...kept, file: sourceFile };
}

/** A stub file that can change what another file finds by name: a script, or a global or module declaration. */
function declaresGlobally(sourceFile: ts.SourceFile): boolean {
  if (!ts.isExternalModule(sourceFile)) return true;
  return sourceFile.statements.some(
    (statement) =>
      ts.isModuleDeclaration(statement) &&
      (ts.isStringLiteral(statement.name) || (statement.flags & ts.NodeFlags.GlobalAugmentation) !== 0)
  );
}

/**
 * The diagnostics of the stub's own files (`treeFiles`), each file's from
 * `hold` where it still answers, sorted as `ts.getPreEmitDiagnostics` sorts
 * them. A file outside the stub is not checked: no record reads a diagnostic
 * of one (closures and repairs are the stub's files only).
 */
export function stubDiagnostics(
  hold: CaptureHold,
  program: ts.Program,
  stubDir: string,
  treeFiles: string[],
  context: string
): ts.Diagnostic[] {
  const kept = hold.selfCheck;
  const tree = new Set(treeFiles.map((file) => path.resolve(file)));
  const outside = new Map<string, ts.SourceFile>();
  for (const sourceFile of program.getSourceFiles()) {
    if (!tree.has(path.resolve(sourceFile.fileName))) outside.set(sourceFile.fileName, sourceFile);
  }
  let reusable =
    kept !== undefined &&
    kept.context === context &&
    kept.outside.size === outside.size &&
    [...outside].every(([fileName, sourceFile]) => kept.outside.get(fileName) === sourceFile);
  const sources = treeFiles.map((file) => program.getSourceFile(file)).filter((s): s is ts.SourceFile => !!s);
  if (reusable) {
    // A file changed from what was last checked, either side of the change
    // declaring something global, changes what other files find by name.
    for (const sourceFile of sources) {
      const last = kept!.byFile.get(path.relative(stubDir, sourceFile.fileName))?.[0];
      if (!last || last.text === sourceFile.text) continue;
      if (declaresGlobally(sourceFile) || declaresGlobally(ts.createSourceFile(sourceFile.fileName, last.text, ts.ScriptTarget.Latest))) {
        reusable = false;
      }
    }
  }
  const byFile = new Map<string, Array<{ text: string; diagnostics: KeptDiagnostic[] }>>();
  const diagnostics: ts.Diagnostic[] = [];
  for (const sourceFile of sources) {
    const rel = path.relative(stubDir, sourceFile.fileName);
    const texts = reusable ? (kept!.byFile.get(rel) ?? []) : [];
    const was = texts.find((entry) => entry.text === sourceFile.text);
    let own: KeptDiagnostic[];
    if (was) {
      own = was.diagnostics;
      hold.counts.check_kept += 1;
    } else {
      own = [...program.getSyntacticDiagnostics(sourceFile), ...program.getSemanticDiagnostics(sourceFile)].map(keep);
      hold.counts.checked += 1;
    }
    const entry = { text: sourceFile.text, diagnostics: own };
    byFile.set(rel, [entry, ...texts.filter((other) => other !== was)].slice(0, 2));
    for (const diagnostic of own) diagnostics.push(restore(diagnostic, sourceFile));
  }
  hold.selfCheck = { context, byFile, outside };
  return [...ts.sortAndDeduplicateDiagnostics(diagnostics)];
}
