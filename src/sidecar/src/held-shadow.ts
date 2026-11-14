/**
 * The shadow tree a held-open compiler reads: a copy of one service's program
 * that lives outside the checkout and loads on TypeScript 7.
 *
 * The scan's loader builds the program on TypeScript 6 (repo tsconfig, Deno
 * graph, workspace links). TypeScript 7's command line refuses options the
 * repo may set (`baseUrl`, `moduleResolution: node`, `target: es5`,
 * `esModuleInterop: false`), and a config outside the repo cannot govern files
 * inside it, so the held compiler never reads the repo's own config or files.
 * It reads this tree instead:
 *
 * - `w/<repo-relative path>`: byte copies of the service's own files;
 * - `v/<absolute path>`: every other program file (installed packages,
 *   runtime types), with each import, export, import type, augmentation and
 *   `types` reference pinned to the copy of the file TypeScript 6 resolved it
 *   to, so one package name that resolves to two versions stays two files;
 * - `w/tsconfig.json`: generated, `moduleResolution: bundler`, exact `paths`
 *   for the bare specifiers the own files use, every non-library program file
 *   named in `files` (so `@types` the loader included on its own are
 *   explicit), and the library files TypeScript 6 loaded named in `lib`.
 *
 * Own = under the scanned repo, not under a `node_modules` segment, not under
 * `.carrick/`. A bare import added later is not in `paths` (it needs a new
 * shadow).
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import { ts } from 'ts-morph';
import type { ProjectLoader } from './project-loader.js';
import { WriteGuard, judgeTscPath } from './capture/index.js';

/** What `write_shadow` answers. */
export interface ShadowResult {
  /** Absolute path of the generated `w/tsconfig.json`. */
  tsconfig: string;
  /** The service's own files, repo-relative (to the scan root), sorted. */
  own_files: string[];
  /** Program files copied to `v/`. */
  vendored: number;
  /** Specifiers and references rewritten inside those copies. */
  rewritten: number;
  /** Specifiers TypeScript 6 left unresolved or resolved outside the program. */
  unresolved: number;
  /** A bare specifier two own files resolve to different files: reported, not solved. */
  path_conflicts: Array<{ spec: string; a: string; b: string; from: string }>;
  /** The bare specifiers the own files use, as written to `paths`. */
  bare_specifiers: string[];
  /** Some own file imports a package named `hono` that resolved into the program. */
  hono: boolean;
  /** The native TypeScript 7 executable, or null when only TypeScript 6's tsc is installed. */
  native_tsc: string | null;
}

/** The platform package's executable, `@typescript/typescript-<platform>-<arch>/lib/tsc`. */
const NATIVE_TSC = /[\\/]@typescript[\\/]typescript-[^\\/]+[\\/]lib[\\/]tsc(\.exe)?$/;

/** `judgeTscPath()` when it is the native executable, else null (no held compiler). */
export function nativeTscPath(tscPath: string = judgeTscPath()): string | null {
  return NATIVE_TSC.test(tscPath) ? tscPath : null;
}

/**
 * The compiler's internal members the loader's program carries and its public
 * typings omit (the spike read them the same way): a file's import literals and
 * module augmentations, and the program's per-usage resolution.
 */
interface InternalProgram {
  getResolvedModule(
    sf: ts.SourceFile,
    name: string,
    mode: ts.ResolutionMode
  ): { resolvedModule?: { resolvedFileName: string; packageId?: { name: string } } } | undefined;
  getDefaultResolutionModeForFile(sf: ts.SourceFile): ts.ResolutionMode;
  getResolvedTypeReferenceDirective?(
    sf: ts.SourceFile,
    name: string,
    mode: ts.ResolutionMode
  ): { resolvedTypeReferenceDirective?: { resolvedFileName?: string } } | undefined;
}
interface InternalSourceFile {
  imports?: ts.StringLiteralLike[];
  moduleAugmentations?: ts.StringLiteralLike[];
}

const MISSING = (spec: string): string => `__held_unresolved__/${spec.replace(/[^A-Za-z0-9@/._-]/g, '_')}`;

/** A `/// <reference types>` or import target written for a vendored file: relative, JS-spelled for declaration files. */
function relativeSpecifier(fromFile: string, toFile: string): string {
  let rel = path.relative(path.dirname(fromFile), toFile).split(path.sep).join('/');
  if (!rel.startsWith('.')) rel = `./${rel}`;
  return rel.replace(/\.d\.ts$/, '.js').replace(/\.d\.mts$/, '.mjs').replace(/\.d\.cts$/, '.cjs');
}

function isLibraryFile(program: ts.Program, fileName: string): boolean {
  const sf = program.getSourceFile(fileName);
  if (sf && program.isSourceFileDefaultLibrary(sf)) return true;
  return (
    /[\\/]typescript[\\/]lib[\\/]lib\.[^/\\]*\.d\.ts$/.test(fileName) ||
    /[\\/]@ts-morph[\\/]common[\\/].*lib\.[^/\\]*\.d\.ts$/.test(fileName)
  );
}

/**
 * Write the shadow tree of the loader's default program under `outDir` (which
 * must be outside the scan root). Reads the program; changes nothing in the
 * checkout.
 */
export function writeShadow(loader: ProjectLoader, outDir: string): ShadowResult {
  const scanRoot = loader.getScanRoot();
  const program = loader.getProject().getProgram().compilerObject;
  const internal = program as unknown as InternalProgram;
  const roots = [scanRoot, safeRealpath(scanRoot)].filter((r, i, all) => all.indexOf(r) === i);
  const underRoot = (file: string): string | undefined => roots.find((r) => file.startsWith(r + path.sep));
  const isOwn = (file: string): boolean => {
    const root = underRoot(file);
    if (root === undefined) return false;
    const segments = file.slice(root.length + 1).split(path.sep);
    return !segments.includes('node_modules') && segments[0] !== '.carrick';
  };
  const ownRelative = (file: string): string => path.relative(underRoot(file) ?? scanRoot, file);
  const guard = WriteGuard.of({ dirs: [outDir], protect: [scanRoot, loader.getRepoRoot()] });
  const own = path.join(outDir, 'w');
  const vendoredRoot = path.join(outDir, 'v');
  const shadowPath = (file: string): string =>
    isOwn(file) ? path.join(own, ownRelative(file)) : path.join(vendoredRoot, file);
  const inProgram = new Set(program.getSourceFiles().map((sf) => sf.fileName));

  const paths: Record<string, string[]> = {};
  const pathConflicts: ShadowResult['path_conflicts'] = [];
  const ownFiles: string[] = [];
  let vendored = 0;
  let rewritten = 0;
  let unresolved = 0;
  let hono = false;

  for (const sf of program.getSourceFiles()) {
    const file = sf.fileName;
    if (isLibraryFile(program, file)) continue;
    const literals: Array<{ node: ts.StringLiteralLike; text: string; to?: string; pkg?: string; augmentation: boolean; synthesized: boolean }> = [];
    const { imports = [], moduleAugmentations = [] } = sf as unknown as InternalSourceFile;
    for (const node of [...imports, ...moduleAugmentations]) {
      if (!ts.isStringLiteralLike(node)) continue;
      const augmentation = moduleAugmentations.includes(node);
      if (augmentation && !ts.isExternalModule(sf)) continue; // an ambient declaration, not a resolution
      const mode = program.getModeForUsageLocation(sf, node);
      const resolved = internal.getResolvedModule(sf, node.text, mode)?.resolvedModule;
      literals.push({
        node,
        text: node.text,
        to: resolved?.resolvedFileName,
        pkg: resolved?.packageId?.name,
        augmentation,
        synthesized: node.pos < 0 || node.end < 0 || !node.parent,
      });
    }
    if (isOwn(file)) {
      ownFiles.push(ownRelative(file));
      const out = shadowPath(file);
      guard.mkdir(path.dirname(out));
      guard.copyFile(file, out);
      for (const lit of literals) {
        if (lit.text.startsWith('.') || path.isAbsolute(lit.text)) continue;
        if (!lit.to || !inProgram.has(lit.to)) continue; // unresolved stays unresolved: no `paths` entry
        if (isHonoFile(loader, lit.to, lit.pkg)) hono = true;
        const target = shadowPath(lit.to);
        const known = paths[lit.text];
        if (known && known[0] !== target) pathConflicts.push({ spec: lit.text, a: known[0], b: target, from: ownRelative(file) });
        else paths[lit.text] = [target];
      }
      continue;
    }
    // A vendored copy: every specifier pinned to the copy TypeScript 6 chose.
    let text = sf.text;
    const edits: Array<{ start: number; end: number; text: string }> = [];
    for (const lit of literals) {
      if (lit.synthesized) continue;
      let spec: string;
      if (lit.to && inProgram.has(lit.to)) spec = relativeSpecifier(shadowPath(file), shadowPath(lit.to));
      else {
        spec = MISSING(lit.text);
        unresolved++;
      }
      edits.push({ start: lit.node.getStart(sf) + 1, end: lit.node.end - 1, text: spec });
    }
    for (const ref of sf.typeReferenceDirectives ?? []) {
      const mode = ref.resolutionMode ?? internal.getDefaultResolutionModeForFile(sf);
      const target = internal.getResolvedTypeReferenceDirective?.(sf, ref.fileName, mode)?.resolvedTypeReferenceDirective?.resolvedFileName;
      // The whole `/// <reference types="..." />` line becomes a path reference to the copy.
      const lineStart = text.lastIndexOf('\n', ref.pos) + 1;
      const newline = text.indexOf('\n', ref.end);
      const lineEnd = newline === -1 ? text.length : newline;
      const replacement =
        target && inProgram.has(target)
          ? `/// <reference path="${relativeSpecifier(shadowPath(file), shadowPath(target)).replace(/\.js$/, '.d.ts').replace(/\.mjs$/, '.d.mts').replace(/\.cjs$/, '.d.cts')}" />`
          : `// held: unresolved types reference ${ref.fileName}`;
      edits.push({ start: lineStart, end: lineEnd, text: replacement });
    }
    edits.sort((a, b) => b.start - a.start);
    for (const edit of edits) text = text.slice(0, edit.start) + edit.text + text.slice(edit.end);
    rewritten += edits.length;
    const out = shadowPath(file);
    guard.mkdir(path.dirname(out));
    guard.writeFile(out, text);
    vendored++;
  }

  const options = program.getCompilerOptions();
  // A repo that sets no `lib` gets the default library files for its target (DOM, es2022, ...). `lib: []`
  // would leave only what `@types/node` references, so the files TypeScript 6 loaded are named. `*.full`
  // is a file, not a `--lib` name, and TypeScript 7 refuses it; its parts are listed.
  const libraryNames = program
    .getSourceFiles()
    .map((sf) => sf.fileName)
    .filter((f) => isLibraryFile(program, f))
    .map((f) => path.basename(f).replace(/^lib\./, '').replace(/\.d\.ts$/, ''))
    .filter((name) => !name.endsWith('.full'));
  const lib = options.lib ? options.lib.map((l) => l.replace(/^lib\./, '').replace(/\.d\.ts$/, '')) : libraryNames;
  // Every non-library program file is a root, so `@types` the loader included on its own and the
  // synthesised JSX runtime import's target stay in the program.
  const files = program.getSourceFiles().filter((sf) => !isLibraryFile(program, sf.fileName)).map((sf) => shadowPath(sf.fileName));
  const jsx = ({ 1: 'preserve', 2: 'react', 3: 'react-native', 4: 'react-jsx', 5: 'react-jsxdev' } as Record<number, string>)[options.jsx ?? 0];
  const passed = [
    'noImplicitAny', 'strictNullChecks', 'noUnusedLocals', 'noUnusedParameters', 'exactOptionalPropertyTypes',
    'noImplicitOverride', 'noImplicitReturns', 'noFallthroughCasesInSwitch', 'noUncheckedIndexedAccess',
    'useUnknownInCatchVariables', 'verbatimModuleSyntax', 'isolatedModules', 'experimentalDecorators',
    'emitDecoratorMetadata', 'useDefineForClassFields', 'noPropertyAccessFromIndexSignature',
  ] as const;
  const compilerOptions = {
    // Options TypeScript 7 removed (baseUrl, node10 resolution, es5, esModuleInterop: false) are never carried.
    target: 'esnext', module: 'esnext', moduleResolution: 'bundler', strict: options.strict ?? false,
    allowJs: !!options.allowJs, checkJs: !!options.checkJs, skipLibCheck: !!options.skipLibCheck,
    resolveJsonModule: !!options.resolveJsonModule,
    ...(jsx ? { jsx } : {}), ...(options.jsxImportSource ? { jsxImportSource: options.jsxImportSource } : {}),
    lib, allowImportingTsExtensions: true, noEmit: true,
    // `false` (the loader's reading of `esModuleInterop: false`) is refused by TypeScript 7 (TS5108); the default is true.
    ...(options.allowSyntheticDefaultImports === false ? {} : { allowSyntheticDefaultImports: true }),
    types: options.types ?? [], noUncheckedSideEffectImports: options.noUncheckedSideEffectImports ?? false,
    ...Object.fromEntries(
      passed.filter((k) => (options as Record<string, unknown>)[k] !== undefined).map((k) => [k, (options as Record<string, unknown>)[k]])
    ),
    paths,
  };
  const tsconfig = path.join(own, 'tsconfig.json');
  guard.mkdir(own);
  guard.writeFile(tsconfig, JSON.stringify({ compilerOptions, files, include: [] }, null, 1));

  return {
    tsconfig,
    own_files: ownFiles.sort(),
    vendored,
    rewritten,
    unresolved,
    path_conflicts: pathConflicts,
    bare_specifiers: Object.keys(paths).sort(),
    hono,
    native_tsc: nativeTscPath(),
  };
}

/** The resolved file belongs to a package named `hono`: by the resolver's package id, the Deno graph, or the install path. */
function isHonoFile(loader: ProjectLoader, file: string, packageId: string | undefined): boolean {
  return packageId === 'hono' || loader.packageDirectoryOf(file)?.name === 'hono' || /[\\/]node_modules[\\/]hono[\\/]/.test(file);
}

function safeRealpath(p: string): string {
  try {
    return fs.realpathSync(p);
  } catch {
    return p;
  }
}
