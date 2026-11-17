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
import {
  WriteGuard,
  absentBodyApplies,
  absentBodyRule,
  graphqlComparandLeafRule,
  graphqlComparandRule,
  graphqlExpectedRule,
  graphqlDeclaredDeclarations,
  graphqlDeclaredRule,
  graphqlGenericDeclarations,
  graphqlLeafWalkDeclarations,
  graphqlReachableFields,
  graphqlRootLeaf,
  graphqlScalarWireRule,
  judgeTscPath,
  jsonWireDeclarations,
  jsonWireRule,
  judgeTolerances,
  topTypeDeclarations,
  fitOptionsOf,
  judgeFitOptions,
  wireFormFor,
  type FitOptions,
  type GraphqlLeaf,
  type GraphqlRoot,
  type GraphqlTypes,
  type JudgeTolerances,
} from './capture/index.js';

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
  /**
   * The judge's JSON wire form as declarations a caller appends to a file:
   * `jsonWireDeclarations('__carrick_')` and `type __carrick_Wire<S, E>`, the
   * judge's own rule (`jsonWireRule`). One writer, so a held check and the
   * scan's judge answer "what travels" the same way.
   */
  wire: string[];
  /** The judge's message rules for a socket payload, as declarations to append beside `tolerances` (`heldMessageWire`). */
  message: string[];
  /** The judge's three tolerances, read from its own code (`judgeTolerances('__carrick_')`): declarations to append beside `wire`. */
  tolerances: JudgeTolerances;
  /**
   * Present when the request carried the service's GraphQL table: the judge's
   * GraphQL rules as declarations (`lines`), the one table declaration they
   * read (`types_decl`, null when no root is read in the scalar form), and the
   * leaf of each root the judge reads in the scalar form, by `Type.field`
   * (`wireFormFor`). A root with no leaf is checked in the JSON form.
   */
  graphql?: HeldGraphql;
  /** The fit options this shadow program was written with, and the judge's: a held check is armed only when they agree. */
  options: { service: FitOptions; judge: FitOptions };
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
export function writeShadow(
  loader: ProjectLoader,
  outDir: string,
  graphql?: { types: GraphqlTypes; roots: GraphqlRoot[] }
): ShadowResult {
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

  for (const sf of program.getSourceFiles()) {
    const file = sf.fileName;
    if (isLibraryFile(program, file)) continue;
    const literals: Array<{ node: ts.StringLiteralLike; text: string; to?: string; augmentation: boolean; synthesized: boolean }> = [];
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
    'emitDecoratorMetadata', 'useDefineForClassFields', 'strictFunctionTypes', 'noPropertyAccessFromIndexSignature',
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
    wire: heldWire(),
    ...(graphql === undefined ? {} : { graphql: heldGraphql(graphql.types, graphql.roots) }),
    tolerances: judgeTolerances('__carrick_'),
    message: heldMessageWire(),
    options: { service: fitOptionsOf(compilerOptions), judge: judgeFitOptions() },
    native_tsc: nativeTscPath(),
  };
}

/** The wire declarations and the one rule type a held check appends (prefix `__carrick_`). */
export function heldWire(): string[] {
  return [
    ...jsonWireDeclarations('__carrick_'),
    `type __carrick_Wire<S, E> = ${jsonWireRule('S', 'E', '__carrick_')};`,
  ];
}

/** What a held GraphQL check appends (see `ShadowResult.graphql`). */
export interface HeldGraphql {
  lines: string[];
  types_decl: string | null;
  leaves: Record<string, GraphqlLeaf>;
}

/**
 * The judge's GraphQL rules as declarations a held check appends beside `wire`
 * and `tolerances` (prefix `__carrick_`). Every rule is the judge's own text
 * (`check-probe.ts`), so a held check and the probe answer from one writer:
 *
 *  - `__carrick_GqlSent<S, E>`: the JSON form, `S` the resolver's value and
 *    `E` the caller's expected type;
 *  - `__carrick_GqlSentS<S, E, R>`: the scalar form, `R` the root's leaf
 *    (`leaves`); it reads `__carrick_GqlTypes` (`types_decl`). A
 *    declared-void caller states no contract, and is kept; where the wire
 *    form does not assign, the judge's declared line decides (carrick#4726).
 *
 * `scalar` adds the scalar form's lines; they name `__carrick_GqlTypes`, so
 * they are written only when the table is declared.
 */
export function heldGraphqlWire(scalar: boolean): string[] {
  const p = '__carrick_';
  const relaxed = `${p}GqlExpected<E>`;
  const comparand = `${p}GqlComparand<S, E>`;
  const lines = [
    ...graphqlGenericDeclarations(p),
    `type ${p}GqlExpected<E> = ${graphqlExpectedRule('E', p)};`,
    `type ${p}GqlComparand<S, E> = ${graphqlComparandRule('S', 'E', relaxed, p)};`,
    `type ${p}GqlSent<S, E> = ${p}IsVoid<E> extends true ? E : ${p}Wire<${comparand}, ${relaxed}>;`,
  ];
  if (!scalar) return lines;
  const leaf = `${p}GqlComparandLeaf<S, E, R>`;
  const form = `${p}GqlWireForm<S, E, R>`;
  return [
    ...lines,
    ...graphqlLeafWalkDeclarations(p),
    `type ${p}GqlComparandLeaf<S, E, R> = ${graphqlComparandLeafRule('S', 'E', relaxed, 'R', p)};`,
    `type ${p}GqlWireForm<S, E, R> = ${graphqlScalarWireRule(comparand, relaxed, leaf, p)};`,
    `type ${p}GqlWire<S, E, R> = ${form};`,
    // carrick#4726: the judge's declared line. Where the wire form does not
    // assign, the held form sends what the judge's decisive line sends: the
    // wire type with the fields the root record lacks or converts in the type
    // the served schema declares.
    ...graphqlDeclaredDeclarations(p),
    `type ${p}GqlSentS<S, E, R> = ${p}IsVoid<E> extends true ? E : ${graphqlDeclaredRule(`${p}GqlWire<S, E, R>`, relaxed, leaf, p)};`,
  ];
}

/**
 * The GraphQL answer for a service's table and roots: a leaf only for a root
 * whose form the judge reads as `graphql_scalars` (`wireFormFor`), the table
 * every such leaf reaches, and the declarations.
 */
export function heldGraphql(types: GraphqlTypes, roots: GraphqlRoot[]): HeldGraphql {
  const leaves: Record<string, GraphqlLeaf> = {};
  const reachable: Record<string, Record<string, unknown>> = {};
  for (const root of roots) {
    if (wireFormFor('graphql', 'response', 'producer', { graphql: { types, root } }) !== 'graphql_scalars') continue;
    const leaf = graphqlRootLeaf(types, root)!;
    leaves[`${root.type}.${root.field}`] = leaf;
    Object.assign(reachable, graphqlReachableFields(types, leaf));
  }
  const scalar = Object.keys(leaves).length > 0;
  const table = Object.fromEntries(Object.keys(reachable).sort().map((name) => [name, reachable[name]]));
  return {
    lines: heldGraphqlWire(scalar),
    types_decl: scalar ? `type __carrick_GqlTypes = ${JSON.stringify(table)};` : null,
    leaves,
  };
}

/**
 * The judge's message rules (a socket payload: the emitter is `sent`, the
 * listener `expected`) as declarations a held check appends beside `tolerances`
 * (prefix `__carrick_`). Every rule is the judge's own text (`check-probe.ts`):
 *
 *  - `__carrick_IsAny`, `__carrick_IsUnknown`, `__carrick_IsNever`
 *    (`topTypeDeclarations`);
 *  - `__carrick_MsgSent<S, E>`, `S` the emitter's payload and `E` the
 *    listener's parameter. Where the judge reads a gate unverifiable (a top,
 *    bottom or void type on either side) the answer is `E`, so the check is
 *    silent; otherwise it is the sent type with the absent body dropped where
 *    the listener requires none (`absentBodyRule`, tolerant form, because the
 *    listener's `absent_body_tolerated` mark is not stored: silent where the
 *    judge without the mark would speak). A socket payload travels as
 *    declared (`wireFormFor`), so no wire transform is applied.
 *
 * It reads `__carrick_IsVoid` (`tolerances.declarations`).
 */
export function heldMessageWire(): string[] {
  const p = '__carrick_';
  if (!absentBodyApplies('socket', 'response')) throw new Error('the absent-body rule no longer reads a socket payload');
  const wire = wireFormFor('socket', 'response', 'consumer');
  const sent = wire === 'json' ? `${p}Wire<S, E>` : 'S';
  const body = absentBodyRule(sent, 'E', true);
  const top = (t: string): string => `${p}IsAny<${t}> extends true ? true : ${p}IsUnknown<${t}> extends true ? true : ${p}IsNever<${t}> extends true ? true : ${p}IsVoid<${t}>`;
  return [
    ...topTypeDeclarations(p),
    `type ${p}MsgSilent<T> = ${top('T')};`,
    `type ${p}MsgSent<S, E> = ${p}MsgSilent<E> extends true ? E : ${p}MsgSilent<S> extends true ? E : ${body};`,
  ];
}

function safeRealpath(p: string): string {
  try {
    return fs.realpathSync(p);
  } catch {
    return p;
  }
}
