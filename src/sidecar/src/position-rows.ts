/**
 * Per-position compiler rows for a service (carrick#2405, move 2 of the fact
 * layer). Record-only: nothing in a scan reads the file yet.
 *
 * For every source file the scanner sends, one row for each position of six
 * kinds, each holding what the compiler knows there: the symbol, the
 * declaration it is attributed to (file and line), the package that declares
 * it and the type text.
 *
 * | kind      | node                                              | symbol of                 | type of        |
 * |-----------|---------------------------------------------------|---------------------------|----------------|
 * | `call`    | call, `new`, tagged template                      | the callee's name         | the call       |
 * | `member`  | property or element access                        | the name or the argument  | the access     |
 * | `compare` | `=== !== == != < > <= >= instanceof in`           | the left operand          | the left operand |
 * | `ident`   | an identifier in expression position              | itself                    | itself         |
 * | `decl`    | a `call` or `member` on a union of declaring types | one of those declarations | nothing       |
 * | `jsx`     | a JSX opening or self-closing element's tag        | the tag's name           | the tag        |
 *
 * A `decl` row (version 3, carrick#2448) follows the `call` or `member` row
 * it belongs to and has its span: one per
 * declaration of the member when the receiver is a union whose constituents
 * declare it at more than one place (`(pick ? a : b).get()` with two
 * classes). That row names only the first of them. Each constituent's
 * declaration is its own member's, or, for a member with none (a mapped
 * type's), the declaration of the constituent type itself. Sorted by file and
 * position; past {@link DECL_CAP} the row is one, with {@link FLAG_TOO_MANY}
 * and nothing else. `type_text` is `null`.
 *
 * Version 4 (carrick#2561) dropped the `impl` rows of versions 2 and 3: the
 * go-to-implementation search behind them was most of the walk's time, and
 * nothing read them. Implementations are asked for where discovery stops
 * (carrick#2432).
 *
 * A row's key is `(start, end, kind)`, except that several `decl` rows share
 * one: TypeScript `getStart()`/`getEnd()` of the
 * node, in UTF-16 code units from the start of the file, which is what the scanner's `sidecar_position`
 * gives for an SWC span. Every path written is relative to the scanned repo's
 * root, with `/` separators.
 *
 * Each file is read in the program of the project that owns it
 * (`ProjectLoader.projectKeyFor`, carrick#1604), one owner's program at a
 * time: a group's program is released before the next one is built. A file
 * the owner's program does not take (JavaScript under a config without
 * `allowJs`) is read after every owner, in that owner's `scripts` program
 * (`ProjectLoader.scriptProjectFor`, carrick#2413), with the owner's root
 * declaration files beside it so that the globals they declare are declared.
 *
 * The file format is pinned on the scanner side by `src/position_rows.rs`.
 */

import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { ts, type Project } from 'ts-morph';
import { WriteGuard, installedPackageDirectory, isDeclarationFileName, moduleExport, toPosix } from './capture/index.js';
import { attributedDeclaration, installedPackageOf, typeDeclaration } from './declaration-attribution.js';
import { typeToStringAsDeclared } from './print-type.js';
import type { ProjectLoader } from './project-loader.js';
import type { PositionRowsCounts } from './types.js';

/** The format's version, written in the header. */
export const POSITION_ROWS_VERSION = 5;

/** The type is `any`, declared or inferred. Never set with the error type. */
export const FLAG_ANY = 1;
/** The type is the compiler's error type: something did not resolve. */
export const FLAG_ERROR_TYPE = 2;
/** The checker threw at this node; the row holds nothing else. */
export const FLAG_CHECKER_THREW = 4;
/** The declaration is outside the checkout, not the default library and in no installed package. */
export const FLAG_DECLARATION_UNNAMED = 8;
/** A `decl` row standing for more than {@link DECL_CAP} rows; it names none. */
export const FLAG_TOO_MANY = 16;
/**
 * The declaration is published under the row's symbol name: its module
 * exports it by that name, or it is a global (a script's top-level
 * declaration, or one in `declare global`). A member of a class, interface,
 * type literal or object literal never is (carrick#2508).
 */
export const FLAG_PUBLISHED = 32;
/** The declaration is what its module exports as `export =` or `default` (carrick#2508). */
export const FLAG_MODULE_DEFAULT = 64;
/**
 * A `call` row only: the call's declared return is the result of the
 * function it is handed (`ReturnType<F>`, a bare type parameter the handed
 * function returns, or an awaitable of one), so the library runs it and
 * hands its result back (carrick#2634). Set when the call passes at least
 * one argument at a function-typed parameter and every such parameter
 * meets that return. The last free bit of the `u8`: the next flag widens
 * the field and bumps the format. Pinned to `FLAG_RUNS_HANDED` in
 * `src/position_rows.rs` by `position-rows-runs-handed.test.ts`.
 */
export const FLAG_RUNS_HANDED = 128;

/** The most `decl` rows one position lists. */
export const DECL_CAP = 16;

/** The kinds {@link kindOf} gives a node; `decl` and `jsx` rows are written beside them. */
export type NodeKind = 'call' | 'member' | 'compare' | 'ident';
export type PositionKind = NodeKind | 'decl' | 'jsx' | 'typed_decl';

/** `[kind, start, end, symbol, decl_file, decl_line, decl_package, type_text, flags]` */
export type PositionRow = [
  PositionKind,
  number,
  number,
  string | null,
  string | null,
  number | null,
  string | null,
  string | null,
  number,
];

export interface PositionRowsOptions {
  loader: ProjectLoader;
  /** Absolute paths, written in this order. */
  files: string[];
  scanRoot: string;
  outPath: string;
  service: string;
  /** Called after each file is walked or skipped. */
  onFile?: () => void;
}

const COMPARE_OPERATORS = new Set<ts.SyntaxKind>([
  ts.SyntaxKind.EqualsEqualsEqualsToken,
  ts.SyntaxKind.ExclamationEqualsEqualsToken,
  ts.SyntaxKind.EqualsEqualsToken,
  ts.SyntaxKind.ExclamationEqualsToken,
  ts.SyntaxKind.LessThanToken,
  ts.SyntaxKind.GreaterThanToken,
  ts.SyntaxKind.LessThanEqualsToken,
  ts.SyntaxKind.GreaterThanEqualsToken,
  ts.SyntaxKind.InstanceOfKeyword,
  ts.SyntaxKind.InKeyword,
]);

/** One line of the file per source file. */
interface FileLine {
  f: string;
  h?: string;
  skip?: 'unreadable' | 'not_in_program';
  r: PositionRow[];
}

/**
 * Walk `files`, write their rows to `outPath` and say what was written.
 *
 * The file is written beside `outPath` as `<outPath>.partial` and renamed
 * over it, so a walk that dies leaves no half-written rows file. Every write
 * goes through a {@link WriteGuard} over the file's directory, which refuses
 * a directory inside the scanned trees anywhere but beneath `.carrick`.
 */
export function writePositionRows(options: PositionRowsOptions): PositionRowsCounts {
  const { loader, files, scanRoot, outPath, service } = options;
  const outDir = path.dirname(outPath);
  const guard = WriteGuard.of({ dirs: [outDir], protect: [loader.getRepoRoot(), scanRoot] });
  const paths = new PathNames(scanRoot, (file) => loader.packageDirectoryOf(file));
  const lines = new Array<string>(files.length);
  const counts: PositionRowsCounts = {
    files: files.length,
    rows: 0,
    resolved: 0,
    any: 0,
    error_type: 0,
    jsx: 0,
    bytes: 0,
    load_ms: 0,
    walk_ms: 0,
  };

  // Files in request order within each owner's group; groups in the order
  // their first file appears.
  const groups = new Map<string, number[]>();
  files.forEach((file, index) => {
    const key = loader.projectKeyFor(file);
    groups.set(key, [...(groups.get(key) ?? []), index]);
  });

  // Files an owner's program did not take, by owner, with that program's
  // root declaration files.
  const untaken = new Map<string, { indexes: number[]; declarations: string[] }>();

  /**
   * Add each file of `indexes` to `project`, build its program once, and walk
   * every file the program takes. Returns the files it did not take; every
   * other file is settled, and reported to `onFile`.
   */
  const walkGroup = (
    project: Project,
    indexes: number[],
    hashes: Map<number, string>
  ): { missed: number[]; program: ts.Program } => {
    const loadStarted = performance.now();
    const read = new Map<number, string | undefined>();
    for (const index of indexes) {
      const file = files[index];
      if (!hashes.has(index)) {
        let bytes: Buffer;
        try {
          bytes = fs.readFileSync(file);
        } catch {
          lines[index] = fileLine({ f: paths.relative(file), skip: 'unreadable', r: [] });
          options.onFile?.();
          continue;
        }
        hashes.set(index, crypto.createHash('sha256').update(bytes).digest('hex'));
      }
      // A file no program lists is added, as `infer` adds one.
      let sourceFile = project.getSourceFile(file);
      if (!sourceFile) {
        try {
          sourceFile = project.addSourceFileAtPath(file);
        } catch {
          sourceFile = undefined;
        }
      }
      read.set(index, sourceFile?.getFilePath());
    }
    // Built once, after every file the group adds. The language service's
    // program is the one walked, as it was when go-to-implementation answered
    // over it (carrick#2430, dropped in #2561): its rows are pinned.
    const service = project.getLanguageService().compilerObject;
    const program = service.getProgram();
    if (!program) throw new Error('the language service built no program');
    counts.load_ms += performance.now() - loadStarted;

    const walkStarted = performance.now();
    const walker = new PositionWalker(program, paths, loader);
    const missed: number[] = [];
    for (const index of indexes) {
      if (!read.has(index)) continue;
      const fileName = read.get(index);
      const sourceFile = fileName ? program.getSourceFile(fileName) : undefined;
      if (!sourceFile) {
        missed.push(index);
        continue;
      }
      const rows = walker.rowsOf(sourceFile);
      for (const row of rows) {
        counts.rows += 1;
        if (row[3] !== null) counts.resolved += 1;
        if (row[8] & FLAG_ANY) counts.any += 1;
        if (row[8] & FLAG_ERROR_TYPE) counts.error_type += 1;
        if (row[0] === 'jsx') counts.jsx += 1;
      }
      lines[index] = fileLine({ f: paths.relative(files[index]), h: hashes.get(index), r: rows });
      options.onFile?.();
    }
    counts.walk_ms += performance.now() - walkStarted;
    return { missed, program };
  };

  const hashes = new Map<number, string>();
  for (const [key, indexes] of groups) {
    const project = loader.getProjectFor(key);
    const { missed, program } = walkGroup(project, indexes, hashes);
    if (missed.length > 0) {
      const declarations = program.getRootFileNames().filter(isDeclarationFileName);
      untaken.set(key, { indexes: missed, declarations });
    }
    // One owner's program at a time.
    loader.releaseProjectFor(key);
  }

  for (const [key, { indexes, declarations }] of untaken) {
    const loadStarted = performance.now();
    const project = loader.scriptProjectFor(key);
    for (const declaration of declarations) {
      try {
        project.addSourceFileAtPath(declaration);
      } catch {
        // A declaration file that went away is not the walk's to report.
      }
    }
    counts.load_ms += performance.now() - loadStarted;
    for (const index of walkGroup(project, indexes, hashes).missed) {
      lines[index] = fileLine({ f: paths.relative(files[index]), h: hashes.get(index), skip: 'not_in_program', r: [] });
      options.onFile?.();
    }
  }

  const writeStarted = performance.now();
  const header = JSON.stringify({
    v: POSITION_ROWS_VERSION,
    typescript: ts.version,
    service,
    files: files.length,
  });
  const gzipped = zlib.gzipSync(Buffer.from([header, ...lines].join('\n') + '\n', 'utf8'));
  const partial = `${outPath}.partial`;
  guard.mkdir(outDir);
  guard.writeFile(partial, gzipped);
  guard.rename(partial, outPath);
  counts.bytes = gzipped.length;
  counts.walk_ms = Math.round(counts.walk_ms + performance.now() - writeStarted);
  counts.load_ms = Math.round(counts.load_ms);
  return counts;
}

/** What a declaration's file is written as in a row's `decl_file`: `lib:<name>` for the default library, else `PathNames`'. */
export function declaringFileName(program: ts.Program, paths: PathNames, file: ts.SourceFile): string | null {
  return program.isSourceFileDefaultLibrary(file) ? `lib:${path.basename(file.fileName)}` : paths.declaringFile(file.fileName);
}

/** A file's line, with its keys in the pinned order. */
function fileLine(line: FileLine): string {
  return JSON.stringify(line);
}

/** The installed package directory a resolved file lies in, and its name, as the module graph places it. */
export type PackageDirectoryOf = (fileName: string) => { directory: string; name: string } | undefined;

/**
 * Names for the files rows point at, never an absolute path: relative to the
 * scanned repo's root when inside it (its real path too, so a root reached
 * through a link still contains its files), `lib:<name>` for the compiler's
 * default library, `pkg:<name>/<path>` for an installed package outside the
 * root, and nothing otherwise.
 *
 * An installed package outside the root is found by its path's last
 * `node_modules` segment, else by `packageDirectoryOf`: the module graph's
 * answer where the resolver serves a package from a cache whose path has no
 * `node_modules` segment (carrick#2599). Either way the directory must hold a
 * package.json.
 */
export class PathNames {
  private readonly roots: string[];
  private readonly packageNames = new Map<string, string | null>();
  /**
   * Answers by declaring file: a service's rows name a few thousand files
   * hundreds of thousands of times, and the module graph's answer is a scan
   * of its packages (on a Deno service it was 40% of the walk).
   */
  private readonly declaringFiles = new Map<string, string | null>();
  private readonly declaringPackages = new Map<string, string | null>();

  constructor(
    scanRoot: string,
    private readonly packageDirectoryOf: PackageDirectoryOf = () => undefined
  ) {
    const resolved = path.resolve(scanRoot);
    let real = resolved;
    try {
      real = fs.realpathSync.native(resolved);
    } catch {
      // A root that cannot be resolved is named as given.
    }
    this.roots = real === resolved ? [resolved] : [resolved, real];
  }

  /** The root `file` lies under, and its path from there; `undefined` outside every root. */
  private inside(file: string): { root: string; rel: string } | undefined {
    for (const root of this.roots) {
      const rel = path.relative(root, file);
      if (rel === '' || rel === '..' || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)) continue;
      return { root, rel };
    }
    return undefined;
  }

  /** A request file's name: relative to the root, which every file the scanner sends is inside. */
  relative(file: string): string {
    const inside = this.inside(path.resolve(file));
    return toPosix(inside ? inside.rel : path.basename(file));
  }

  /** What a declaring file is written as, or `null` when nothing names it. */
  declaringFile(fileName: string): string | null {
    let named = this.declaringFiles.get(fileName);
    if (named === undefined) {
      named = this.nameDeclaringFile(fileName);
      this.declaringFiles.set(fileName, named);
    }
    return named;
  }

  private nameDeclaringFile(fileName: string): string | null {
    const inside = this.inside(fileName);
    if (inside) return toPosix(inside.rel);
    const installed = installedPackageDirectory(this.roots[0], fileName) ?? this.packageDirectoryOf(fileName);
    if (installed && fs.existsSync(path.join(installed.directory, 'package.json'))) {
      return `pkg:${installed.name}/${toPosix(path.relative(installed.directory, fileName))}`;
    }
    return null;
  }

  /**
   * The package a declaring file belongs to: the module graph's or the path's
   * answer (`installedPackageOf`), else, for a file inside the root, the
   * `name` of the nearest package.json at or above it, up to the root.
   */
  declaringPackage(
    fileName: string,
    packageOf: (filePath: string) => string | undefined
  ): string | null {
    let named = this.declaringPackages.get(fileName);
    if (named === undefined) {
      named = this.nameDeclaringPackage(fileName, packageOf);
      this.declaringPackages.set(fileName, named);
    }
    return named;
  }

  private nameDeclaringPackage(
    fileName: string,
    packageOf: (filePath: string) => string | undefined
  ): string | null {
    const installed = installedPackageOf(fileName, packageOf);
    if (installed) return installed;
    const inside = this.inside(fileName);
    if (!inside) return null;
    let dir = path.dirname(fileName);
    for (;;) {
      const name = this.packageNameIn(dir);
      if (name !== null) return name;
      if (dir === inside.root) return null;
      const parent = path.dirname(dir);
      if (parent === dir) return null;
      dir = parent;
    }
  }

  private packageNameIn(dir: string): string | null {
    let name = this.packageNames.get(dir);
    if (name === undefined) {
      name = null;
      try {
        const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8')) as {
          name?: unknown;
        };
        if (typeof manifest.name === 'string' && manifest.name.length > 0) name = manifest.name;
      } catch {
        // No package.json here, or one with no name.
      }
      this.packageNames.set(dir, name);
    }
    return name;
  }

  /**
   * `import("/abs/path")` in a printed type, written with the declaring
   * file's name instead. The print has no enclosing node, so it names every
   * module by the file that declares it and one type reads the same from
   * every file; it also skips the accessibility search an enclosing node
   * costs (40% of the walk on a 600-file service).
   */
  scrubType(text: string): string {
    if (!text.includes('import("')) return text;
    return text.replace(/import\("([^"]+)"\)/g, (whole, specifier: string) => {
      if (!path.isAbsolute(specifier)) return whole;
      return `import("${this.declaringFile(specifier) ?? '<outside the checkout>'}")`;
    });
  }
}

/** What a name bound by `require()` refers to: the whole module, or one export. */
interface RequireBinding {
  specifier: string;
  /** The file holding the `require` call, which resolves the specifier. */
  from: ts.SourceFile;
  /** `undefined` for the whole module. */
  exportName?: string;
  /** `m` of an `as typeof import("m")` cast on the require, read when the require names no file. */
  castSpecifier?: string;
}

/**
 * The rows of one program's files. The `implementations` action
 * (`implementations.ts`, carrick#2432) reads a call's symbol and names its
 * answers through the same walker, so they join as the rows do.
 */
export class PositionWalker {
  private readonly checker: ts.TypeChecker;
  /** Each module's exports by name, and its `export =` and `default` targets, aliases followed. */
  private readonly moduleExports = new Map<ts.Symbol, { byName: Map<string, ts.Symbol>; wholes: ts.Symbol[] }>();

  constructor(
    private readonly program: ts.Program,
    private readonly paths: PathNames,
    private readonly loader: ProjectLoader
  ) {
    this.checker = program.getTypeChecker();
  }

  rowsOf(sourceFile: ts.SourceFile): PositionRow[] {
    const rows: PositionRow[] = [];
    const visit = (node: ts.Node): void => {
      const kind = kindOf(node);
      if (kind) {
        const row = this.row(kind, node, sourceFile);
        rows.push(row);
        if ((kind === 'call' || kind === 'member') && !(row[8] & FLAG_CHECKER_THREW)) {
          for (const decl of this.declRows(kind, node)) rows.push(['decl', row[1], row[2], ...tail(decl)]);
        }
        if (kind === 'call' && !(row[8] & FLAG_CHECKER_THREW)) {
          const typed = this.typedDeclRow(node, row[1], row[2]);
          if (typed) rows.push(typed);
        }
      }
      if (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node)) {
        rows.push(this.row('jsx', node.tagName, sourceFile));
      }
      ts.forEachChild(node, visit);
    };
    ts.forEachChild(sourceFile, visit);
    return rows;
  }

  private row(kind: Exclude<PositionKind, 'decl' | 'typed_decl'>, node: ts.Node, sourceFile: ts.SourceFile): PositionRow {
    const start = node.getStart(sourceFile);
    const end = node.getEnd();
    try {
      const symbol = this.symbolOf(kind, node);
      const typed = kind === 'compare' ? (node as ts.BinaryExpression).left : node;
      const type = (kind === 'jsx' && this.intrinsicProps(node)) || this.checker.getTypeAtLocation(typed);
      let flags = 0;
      if (type.flags & ts.TypeFlags.Any) {
        flags |= (type as { intrinsicName?: string }).intrinsicName === 'error' ? FLAG_ERROR_TYPE : FLAG_ANY;
      }
      if (kind === 'call' && this.runsHanded(node)) flags |= FLAG_RUNS_HANDED;
      const typeText = this.paths.scrubType(typeToStringAsDeclared(this.checker, type, undefined));
      if (!symbol) return [kind, start, end, null, null, null, null, typeText, flags];
      const declaration =
        attributedDeclaration(() => this.program, symbol.declarations ?? []) ??
        this.ownerDeclaration(kind, node, symbol);
      if (!declaration) return [kind, start, end, ts.symbolName(symbol), null, null, null, typeText, flags];
      // A declaration named through its owner's type is a member.
      if ((symbol.declarations ?? []).includes(declaration)) flags |= this.publication(symbol, declaration);
      // A file's own module symbol is named by the compiler as the quoted
      // absolute file name (`"/<checkout>/pkg/index"`). Name it as `decl_file`
      // names the file. A symbol that merely lives in a file (CommonJS
      // `exports`) keeps its name.
      const symbolName = ts.symbolName(symbol);
      const isModule = declaration.kind === ts.SyntaxKind.SourceFile && symbolName.startsWith('"');
      const declaringFile = declaration.getSourceFile();
      const { declFile, declLine, declPackage } = this.located(
        declaringFile,
        declaration.getStart(declaringFile)
      );
      if (declFile === null) flags |= FLAG_DECLARATION_UNNAMED;
      const name = isModule ? declFile : symbolName;
      return [kind, start, end, name, declFile, declLine, declPackage, typeText, flags];
    } catch {
      return [kind, start, end, null, null, null, null, null, FLAG_CHECKER_THREW];
    }
  }

  /**
   * Whether a call's declared return is the result of the function it is
   * handed (carrick#2634), read from the declared signature through public
   * checker calls. See {@link FLAG_RUNS_HANDED}.
   */
  private runsHanded(node: ts.Node): boolean {
    if (!ts.isCallExpression(node) && !ts.isNewExpression(node)) return false;
    const args = node.arguments ?? [];
    if (args.length === 0) return false;
    try {
      const declaration = this.checker.getResolvedSignature(node)?.declaration;
      if (!declaration || ts.isJSDocSignature(declaration)) return false;
      const declared = this.checker.getSignatureFromDeclaration(declaration);
      if (!declared) return false;
      const own = new Set<ts.Type>(declared.getTypeParameters() ?? []);
      const results = new Map<ts.Type, object>();
      const resultOf = (f: ts.Type): object => {
        let atom = results.get(f);
        if (!atom) results.set(f, (atom = {}));
        return atom;
      };
      const atoms = (t: ts.Type, out: Set<unknown>, depth = 0): Set<unknown> => {
        if (depth > 8) return out;
        if (t.flags & ts.TypeFlags.TypeParameter) {
          if (own.has(t)) out.add(t);
        } else if (t.flags & ts.TypeFlags.UnionOrIntersection) {
          for (const member of (t as ts.UnionOrIntersectionType).types) atoms(member, out, depth + 1);
        } else if (t.flags & ts.TypeFlags.Conditional) {
          const f = resultOfConditional(t as ts.ConditionalType, own);
          if (f) out.add(resultOf(f));
        } else if (isReference(t) && this.isAwaitable(t)) {
          for (const arg of this.checker.getTypeArguments(t)) atoms(arg, out, depth + 1);
        }
        return out;
      };
      const returned = atoms(this.checker.getReturnTypeOfSignature(declared), new Set());
      if (returned.size === 0) return false;
      const parameters = declared.getParameters();
      let handed = 0;
      for (let i = 0; i < args.length; i++) {
        const parameter = parameters[Math.min(i, parameters.length - 1)];
        if (!parameter) continue;
        const isRest =
          parameter.valueDeclaration !== undefined &&
          ts.isParameter(parameter.valueDeclaration) &&
          parameter.valueDeclaration.dotDotDotToken !== undefined;
        if (!isRest && i >= parameters.length) continue;
        let type = this.checker.getTypeOfSymbolAtLocation(parameter, declaration);
        if (isRest) {
          if (!isReference(type)) continue;
          const [element] = this.checker.getTypeArguments(type);
          if (!element) continue;
          type = element;
        }
        const signatures = this.callSignaturesOf(type);
        if (signatures.length === 0) continue;
        handed++;
        const mine = new Set<unknown>();
        if (type.flags & ts.TypeFlags.TypeParameter) mine.add(resultOf(type));
        else for (const signature of signatures) atoms(this.checker.getReturnTypeOfSignature(signature), mine);
        if (![...mine].some((atom) => returned.has(atom))) return false;
      }
      return handed > 0;
    } catch {
      return false;
    }
  }

  /** The call signatures of a parameter's type, a type parameter read through its constraint, `null` and `undefined` skipped. */
  private callSignaturesOf(type: ts.Type): readonly ts.Signature[] {
    let t = type;
    if (t.flags & ts.TypeFlags.TypeParameter) t = this.checker.getBaseConstraintOfType(t) ?? t;
    if (t.flags & ts.TypeFlags.Union) {
      const kept = (t as ts.UnionType).types.filter((m) => !(m.flags & (ts.TypeFlags.Null | ts.TypeFlags.Undefined)));
      if (kept.length !== 1) return [];
      t = kept[0];
    }
    return this.checker.getSignaturesOfType(t, ts.SignatureKind.Call);
  }

  /** The compiler's own awaited type differs from the type: `Promise<R>`, `PromiseLike<R>`. */
  private isAwaitable(t: ts.Type): boolean {
    const awaited = this.checker.getAwaitedType(t);
    return awaited !== undefined && awaited !== t;
  }

  /**
   * An intrinsic element's type: the attributes `JSX.IntrinsicElements`
   * gives its tag. The compiler types an intrinsic tag name itself as the
   * error type, though the name resolves. A tag is intrinsic when it is an
   * identifier that starts with a lowercase letter or holds a `-`, as the
   * language reads it.
   */
  private intrinsicProps(tag: ts.Node): ts.Type | undefined {
    if (!ts.isIdentifier(tag) || !/^[a-z]|-/.test(tag.text)) return undefined;
    const element = tag.parent;
    if (!ts.isJsxOpeningElement(element) && !ts.isJsxSelfClosingElement(element)) return undefined;
    return this.checker.getContextualType(element.attributes);
  }

  /** How rows name a position in a program file: its file, 1-based line and package. */
  located(
    declaringFile: ts.SourceFile,
    position: number
  ): { declFile: string | null; declLine: number; declPackage: string | null } {
    const declFile = declaringFileName(this.program, this.paths, declaringFile);
    const declLine = declaringFile.getLineAndCharacterOfPosition(position).line + 1;
    const declPackage = this.paths.declaringPackage(declaringFile.fileName, (file) => this.loader.packageOf(file));
    return { declFile, declLine, declPackage };
  }

  /**
   * The declaration a member with none is attributed to (carrick#2448): the
   * mapped type node, or the alias naming it, of the receiver's type. A
   * mapped type's members are made by the checker and have no declaration of
   * their own. This is the owner `memberTypeWithoutDeclaration` reads such a
   * member at (type-structural-expander.ts): the type's symbol, else its
   * alias symbol. A union or intersection receiver contributes the one
   * constituent that has the property; with several the row is left as it is.
   */
  private ownerDeclaration(kind: PositionKind, node: ts.Node, symbol: ts.Symbol): ts.Declaration | undefined {
    const receiver = this.receiverOf(kind, node);
    if (!receiver) return undefined;
    const holders = (receiver.isUnionOrIntersection() ? receiver.types : [receiver]).filter(
      (constituent) => constituent.getProperty(symbol.name) !== undefined
    );
    if (holders.length !== 1) return undefined;
    return this.typeDeclaration(holders[0]);
  }

  /** The declaration of a type's symbol, else of its alias symbol: a mapped type node, or the alias naming it. */
  private typeDeclaration(type: ts.Type): ts.Declaration | undefined {
    return typeDeclaration(() => this.program, type);
  }

  /**
   * How `declaration`'s module publishes `symbol`: {@link FLAG_PUBLISHED}
   * when under the symbol's own name (or as a global), and
   * {@link FLAG_MODULE_DEFAULT} when as the module's `export =` or `default`.
   * The module is the nearest ambient `declare module "…"` or the file.
   */
  private publication(symbol: ts.Symbol, declaration: ts.Declaration): number {
    let container: ts.Node | undefined = declaration.parent;
    while (container && !ts.isSourceFile(container) && !ts.isModuleBlock(container)) {
      // Anything but a statement list between the declaration and its module
      // (a class, interface, type or object literal, a function body) makes
      // it a member or a local.
      if (!ts.isVariableDeclarationList(container) && !ts.isVariableStatement(container)) return 0;
      container = container.parent;
    }
    if (!container) return 0;
    const block = ts.isModuleBlock(container) ? container.parent : undefined;
    if (block && block.flags & ts.NodeFlags.GlobalAugmentation) return FLAG_PUBLISHED;
    // A namespace's member is published when the module its file is publishes it.
    if (block && !ts.isStringLiteral(block.name)) return this.publishedByExports(symbol, declaration);
    const moduleSymbol = this.checker.getSymbolAtLocation(block ? block.name : container);
    if (moduleSymbol) return this.publishedIn(moduleSymbol, symbol);
    // A script's top-level declaration is a global.
    return ts.isSourceFile(container) && !ts.isExternalModule(container) ? FLAG_PUBLISHED : 0;
  }

  /** A namespace member, found among the exports of the module it sits in. */
  private publishedByExports(symbol: ts.Symbol, declaration: ts.Declaration): number {
    for (let node: ts.Node | undefined = declaration.parent; node; node = node.parent) {
      if (ts.isModuleDeclaration(node) && ts.isStringLiteral(node.name)) {
        const moduleSymbol = this.checker.getSymbolAtLocation(node.name);
        return moduleSymbol ? this.publishedIn(moduleSymbol, symbol) : 0;
      }
      if (ts.isSourceFile(node)) {
        const moduleSymbol = this.checker.getSymbolAtLocation(node);
        return moduleSymbol ? this.publishedIn(moduleSymbol, symbol) & FLAG_PUBLISHED : 0;
      }
    }
    return 0;
  }

  private publishedIn(moduleSymbol: ts.Symbol, symbol: ts.Symbol): number {
    let exports = this.moduleExports.get(moduleSymbol);
    if (!exports) {
      const resolve = (s: ts.Symbol): ts.Symbol =>
        s.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(s) : s;
      const byName = new Map<string, ts.Symbol>();
      for (const exported of this.checker.getExportsOfModule(moduleSymbol)) {
        byName.set(exported.getName(), resolve(exported));
      }
      const wholes = [ts.InternalSymbolName.ExportEquals, ts.InternalSymbolName.Default]
        .map((key) => moduleSymbol.exports?.get(key as ts.__String))
        .filter((own): own is ts.Symbol => own !== undefined)
        .map(resolve);
      exports = { byName, wholes };
      this.moduleExports.set(moduleSymbol, exports);
    }
    return (
      (exports.byName.get(symbol.getName()) === symbol ? FLAG_PUBLISHED : 0) |
      (exports.wholes.includes(symbol) ? FLAG_MODULE_DEFAULT : 0)
    );
  }

  /** The type of the object a `member` or `call` row's name is read on, if the name is read on one. */
  private receiverOf(kind: PositionKind, node: ts.Node): ts.Type | undefined {
    if (kind !== 'member' && kind !== 'call') return undefined;
    const access = nameNodeOf(kind, node)?.parent;
    if (!access || !(ts.isPropertyAccessExpression(access) || ts.isElementAccessExpression(access))) {
      return undefined;
    }
    return this.checker.getTypeAtLocation(access.expression);
  }

  /**
   * The `decl` rows of a `call` or `member` row (spans unset): one per
   * declaration the receiver's union constituents give the member, when they
   * give more than one (carrick#2448). A constituent's declaration is its
   * member's ({@link attributedDeclaration}), or, for a member with none, the
   * constituent type's own ({@link typeDeclaration}). Two instantiations of
   * one generic class, or one class and `undefined`, give one, and no row.
   * A throw writes one row flagged {@link FLAG_CHECKER_THREW}.
   */
  private declRows(kind: 'call' | 'member', node: ts.Node): PositionRow[] {
    const found = new Set<ts.Declaration>();
    let name: string;
    try {
      const symbol = this.symbolOf(kind, node);
      const receiver = this.receiverOf(kind, node);
      if (!symbol || !receiver || !receiver.isUnion()) return [];
      name = ts.symbolName(symbol);
      for (const constituent of receiver.types) {
        const member = constituent.getProperty(symbol.name);
        if (!member) continue;
        const declaration =
          attributedDeclaration(() => this.program, member.declarations ?? []) ?? this.typeDeclaration(constituent);
        if (declaration) found.add(declaration);
      }
    } catch {
      return [['decl', 0, 0, null, null, null, null, null, FLAG_CHECKER_THREW]];
    }
    if (found.size < 2) return [];
    if (found.size > DECL_CAP) return [['decl', 0, 0, null, null, null, null, null, FLAG_TOO_MANY]];
    const rows = [...found].map((declaration) => {
      const file = declaration.getSourceFile();
      const start = declaration.getStart(file);
      const { declFile, declLine, declPackage } = this.located(file, start);
      const flags = declFile === null ? FLAG_DECLARATION_UNNAMED : 0;
      return { start, row: ['decl', 0, 0, name, declFile, declLine, declPackage, null, flags] as PositionRow };
    });
    rows.sort((a, b) => compareNames(a.row[4], b.row[4]) || a.start - b.start);
    return rows.map(({ row }) => row);
  }

  /**
   * The `typed_decl` row of a call (carrick#2448): the callee is a value
   * (a parameter, a property, a local) whose type is a named function's own
   * (`send: typeof fetch`), so the compiler gives the type a symbol whose
   * declaration is that function's. Written only when the callee symbol is
   * not already that declaration and the type's symbol is named (an
   * anonymous function or type literal is `__function` / `__type`), so a
   * direct call or a method call writes none.
   */
  private typedDeclRow(node: ts.Node, start: number, end: number): PositionRow | undefined {
    if (!ts.isCallExpression(node)) return undefined;
    try {
      const symbol = this.symbolOf('call', node);
      if (!symbol) return undefined;
      const type = this.checker.getNonNullableType(this.checker.getTypeAtLocation(node.expression));
      const typeSymbol = type.symbol;
      if (!typeSymbol || typeSymbol.name.startsWith('__')) return undefined;
      if (type.getCallSignatures().length === 0) return undefined;
      const declaration = this.typeDeclaration(type);
      if (!declaration) return undefined;
      const own = symbol.declarations ?? [];
      if (own.some((d) => (typeSymbol.declarations ?? []).includes(d))) return undefined;
      const file = declaration.getSourceFile();
      const { declFile, declLine, declPackage } = this.located(file, declaration.getStart(file));
      let flags = this.publication(typeSymbol, declaration);
      if (declFile === null) flags |= FLAG_DECLARATION_UNNAMED;
      return ['typed_decl', start, end, ts.symbolName(typeSymbol), declFile, declLine, declPackage, null, flags];
    } catch {
      return undefined;
    }
  }

  /** The row's symbol, following aliases to a target that has a declaration. */
  symbolOf(kind: PositionKind, node: ts.Node): ts.Symbol | undefined {
    let symbol: ts.Symbol | undefined;
    if (kind === 'ident' && ts.isShorthandPropertyAssignment(node.parent) && node.parent.name === node) {
      symbol = this.checker.getShorthandAssignmentValueSymbol(node.parent);
    } else {
      const named = nameNodeOf(kind, node);
      symbol = named ? this.checker.getSymbolAtLocation(named) : undefined;
      if (named && /\.[cm]?tsx?$/.test(node.getSourceFile().fileName)) {
        symbol = this.requiredExport(named, symbol) ?? symbol;
      }
    }
    if (symbol && symbol.flags & ts.SymbolFlags.Alias) {
      const target = this.checker.getAliasedSymbol(symbol);
      // An import of a module that does not resolve aliases nothing: the
      // import itself is all there is.
      if (target.declarations && target.declarations.length > 0) symbol = target;
    }
    return symbol;
  }

  /**
   * In a TypeScript file `const { a } = require("m")` binds `a` as `any` with
   * the binding as its only declaration (TypeScript resolves `require` itself
   * only in JavaScript files), so a trace stops there. Name the export the
   * call resolves to instead (carrick#2454): for the binding itself, and for
   * `m.x` where `m` is a whole-module binding. `undefined` when the name is
   * not such a binding, the specifier is not a literal or does not resolve,
   * or the module has no such export.
   */
  private requiredExport(named: ts.Node, symbol: ts.Symbol | undefined): ts.Symbol | undefined {
    try {
      const binding = symbol ? this.requireBinding(symbol) : undefined;
      if (binding) return this.exportOfRequired(binding);
      // `m.x` / `m["x"]`: the symbol of `x` is unknown on an `any` object.
      const access = named.parent;
      let member: string | undefined;
      let object: ts.Expression | undefined;
      if (ts.isPropertyAccessExpression(access) && access.name === named) {
        member = access.name.text;
        object = access.expression;
      } else if (
        ts.isElementAccessExpression(access) &&
        access.argumentExpression === named &&
        ts.isStringLiteral(named)
      ) {
        member = named.text;
        object = access.expression;
      }
      if (member === undefined || !object || !ts.isIdentifier(object)) return undefined;
      const objectSymbol = this.checker.getSymbolAtLocation(object);
      const whole = objectSymbol ? this.requireBinding(objectSymbol) : undefined;
      if (!whole || whole.exportName !== undefined) return undefined;
      return this.exportOfRequired({ ...whole, exportName: member });
    } catch {
      return undefined;
    }
  }

  /** The `require` binding a symbol is, when its only declaration is one. */
  private requireBinding(symbol: ts.Symbol): RequireBinding | undefined {
    const declarations = symbol.declarations ?? [];
    if (declarations.length !== 1) return undefined;
    const declaration = declarations[0];
    let initializer: ts.Expression | undefined;
    let exportName: string | undefined;
    if (ts.isVariableDeclaration(declaration) && ts.isIdentifier(declaration.name)) {
      initializer = declaration.initializer;
      if (initializer && ts.isPropertyAccessExpression(initializer) && ts.isIdentifier(initializer.name)) {
        exportName = initializer.name.text;
        initializer = initializer.expression;
      }
    } else if (
      ts.isBindingElement(declaration) &&
      ts.isIdentifier(declaration.name) &&
      !declaration.dotDotDotToken &&
      !declaration.initializer &&
      ts.isObjectBindingPattern(declaration.parent) &&
      ts.isVariableDeclaration(declaration.parent.parent)
    ) {
      const key = declaration.propertyName;
      if (key && !ts.isIdentifier(key) && !ts.isStringLiteral(key)) return undefined;
      exportName = key ? key.text : declaration.name.text;
      initializer = declaration.parent.parent.initializer;
    }
    let castSpecifier: string | undefined;
    while (
      initializer &&
      (ts.isParenthesizedExpression(initializer) ||
        ts.isAsExpression(initializer) ||
        ts.isSatisfiesExpression(initializer) ||
        ts.isTypeAssertionExpression(initializer) ||
        ts.isNonNullExpression(initializer))
    ) {
      const type = ts.isNonNullExpression(initializer) || ts.isParenthesizedExpression(initializer) ? undefined : initializer.type;
      if (castSpecifier === undefined && type && ts.isImportTypeNode(type) && type.isTypeOf) {
        const arg = type.argument;
        if (ts.isLiteralTypeNode(arg) && ts.isStringLiteral(arg.literal)) castSpecifier = arg.literal.text;
      }
      initializer = initializer.expression;
    }
    const specifier = initializer ? this.requireSpecifier(initializer) : undefined;
    if (specifier === undefined) return undefined;
    const from = declaration.getSourceFile();
    const binding: RequireBinding = exportName === undefined ? { specifier, from } : { specifier, from, exportName };
    if (castSpecifier !== undefined) binding.castSpecifier = castSpecifier;
    return binding;
  }

  /** `"m"` of a call `require("m")` to the global `require`, else `undefined`. */
  private requireSpecifier(expression: ts.Expression): string | undefined {
    if (!ts.isCallExpression(expression) || expression.arguments.length !== 1) return undefined;
    const callee = expression.expression;
    const argument = expression.arguments[0];
    if (!ts.isIdentifier(callee) || callee.text !== 'require' || !ts.isStringLiteral(argument)) return undefined;
    // The global `require` is declared in a declaration file (or nowhere); a
    // `require` the service writes is a declaration in its own source.
    const own = (this.checker.getSymbolAtLocation(callee)?.declarations ?? []).some(
      (d) => !d.getSourceFile().isDeclarationFile
    );
    return own ? undefined : argument.text;
  }

  private exportOfRequired(binding: RequireBinding): ts.Symbol | undefined {
    const sourceOf = (specifier: string): ts.SourceFile | undefined => {
      const resolved = ts.resolveModuleName(
        specifier,
        binding.from.fileName,
        this.program.getCompilerOptions(),
        this.resolutionHost(),
        undefined,
        undefined,
        ts.ModuleKind.CommonJS
      ).resolvedModule;
      return resolved ? this.program.getSourceFile(resolved.resolvedFileName) : undefined;
    };
    // The require that runs wins; the cast is read only when it names no file.
    const target =
      sourceOf(binding.specifier) ?? (binding.castSpecifier !== undefined ? sourceOf(binding.castSpecifier) : undefined);
    if (!target) return undefined;
    if (binding.exportName === undefined) return this.checker.getSymbolAtLocation(target);
    return moduleExport(this.checker, target, binding.exportName);
  }

  private host: ts.ModuleResolutionHost | undefined;

  private resolutionHost(): ts.ModuleResolutionHost {
    this.host ??= ts.createCompilerHost(this.program.getCompilerOptions());
    return this.host;
  }
}

/** A row's fields after its kind and span. */
function tail(row: PositionRow): [string | null, string | null, number | null, string | null, string | null, number] {
  return [row[3], row[4], row[5], row[6], row[7], row[8]];
}

/** Order two `decl_file` names, `null` last. */
export function compareNames(a: string | null, b: string | null): number {
  if (a === b) return 0;
  if (a === null) return 1;
  if (b === null) return -1;
  return a < b ? -1 : 1;
}

function isReference(t: ts.Type): t is ts.TypeReference {
  return !!(t.flags & ts.TypeFlags.Object) && !!((t as ts.ObjectType).objectFlags & ts.ObjectFlags.Reference);
}

/**
 * The type parameter `F` of a conditional written `F extends (...) => infer R ? R : _`
 * (`ReturnType<F>`): the result of `F`. `Parameters<F>` and the like infer
 * elsewhere and give nothing.
 */
function resultOfConditional(t: ts.ConditionalType, own: ReadonlySet<ts.Type>): ts.Type | undefined {
  if (!(t.checkType.flags & ts.TypeFlags.TypeParameter) || !own.has(t.checkType)) return undefined;
  const node = t.root.node;
  if (!ts.isFunctionTypeNode(node.extendsType)) return undefined;
  const returns = node.extendsType.type;
  if (!ts.isInferTypeNode(returns)) return undefined;
  const yielded = node.trueType;
  if (!ts.isTypeReferenceNode(yielded) || !ts.isIdentifier(yielded.typeName)) return undefined;
  return yielded.typeName.text === returns.typeParameter.name.text ? t.checkType : undefined;
}

/** The kind of row a node gives, if any (exported for the editor-parity test, carrick#2418). */
export function kindOf(node: ts.Node): NodeKind | undefined {
  if (ts.isCallExpression(node) || ts.isNewExpression(node) || ts.isTaggedTemplateExpression(node)) {
    return 'call';
  }
  if (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) return 'member';
  if (ts.isBinaryExpression(node) && COMPARE_OPERATORS.has(node.operatorToken.kind)) return 'compare';
  if (ts.isIdentifier(node) && isExpressionIdentifier(node)) return 'ident';
  return undefined;
}

/** The node whose symbol a row of `kind` names. */
function nameNodeOf(kind: PositionKind, node: ts.Node): ts.Node | undefined {
  switch (kind) {
    case 'call':
      return referenceName(
        ts.isTaggedTemplateExpression(node) ? node.tag : (node as ts.CallExpression | ts.NewExpression).expression
      );
    case 'member':
      return ts.isPropertyAccessExpression(node)
        ? node.name
        : (node as ts.ElementAccessExpression).argumentExpression;
    case 'compare':
      return referenceName((node as ts.BinaryExpression).left);
    case 'ident':
      return node;
    case 'decl':
    case 'typed_decl':
      return undefined;
    case 'jsx':
      // The tag: `<Badge>`, `<ui.Button>`, `<this.Panel>`; a namespaced
      // name (`<svg:path>`) names nothing.
      return ts.isJsxNamespacedName(node) ? undefined : (referenceName(node as ts.Expression) ?? node);
  }
}

/**
 * The name an expression refers by, through the wrappers that do not change
 * what it refers to (`(x)`, `x as T`, `<T>x`, `x!`, `x satisfies T`): an
 * identifier, a member's name or argument, `super`. `undefined` for anything
 * else (a call's result, a literal).
 */
function referenceName(expression: ts.Expression): ts.Node | undefined {
  let inner: ts.Expression = expression;
  while (
    ts.isParenthesizedExpression(inner) ||
    ts.isAsExpression(inner) ||
    ts.isTypeAssertionExpression(inner) ||
    ts.isNonNullExpression(inner) ||
    ts.isSatisfiesExpression(inner)
  ) {
    inner = inner.expression;
  }
  if (ts.isIdentifier(inner) || ts.isPrivateIdentifier(inner)) return inner;
  if (ts.isPropertyAccessExpression(inner)) return inner.name;
  if (ts.isElementAccessExpression(inner)) return inner.argumentExpression;
  if (inner.kind === ts.SyntaxKind.SuperKeyword) return inner;
  return undefined;
}

/**
 * Whether an identifier is in expression position: a reference to a value,
 * including a callee, a member's object and a shorthand property `{ a }`.
 * Not a declaration's or binding's name, a type-position name, an import or
 * export specifier, a property key, a member's name, a label, a meta-property
 * (`import.meta`), or a JSX tag or attribute name.
 */
function isExpressionIdentifier(node: ts.Identifier): boolean {
  const parent = node.parent;
  if (ts.isShorthandPropertyAssignment(parent) && parent.name === node) return true;
  if (ts.isPartOfTypeNode(node)) return false;
  const named = parent as {
    name?: ts.Node;
    propertyName?: ts.Node;
    label?: ts.Node;
    tagName?: ts.Node;
  };
  if (named.name === node || named.propertyName === node || named.label === node) return false;
  if (named.tagName === node) return false;
  if (ts.isQualifiedName(parent) || ts.isMetaProperty(parent)) return false;
  // The tag of a JSX element written as a member (`<ui.Button>`).
  if (ts.isPropertyAccessExpression(parent) && isJsxTagName(parent)) return false;
  return true;
}

function isJsxTagName(node: ts.Node): boolean {
  let tag: ts.Node = node;
  while (ts.isPropertyAccessExpression(tag.parent) && tag.parent.expression === tag) tag = tag.parent;
  const holder = tag.parent as { tagName?: ts.Node };
  return holder.tagName === tag;
}
