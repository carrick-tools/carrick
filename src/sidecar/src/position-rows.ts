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
 * Version 6 added `receiver_type` rows: beside a `member` row,
 * at the span of the object it is read on, the declaration of each named class
 * or interface that object's type holds, when the member is declared in
 * another package than the class. Several share one key, ordered as `decl`.
 *
 * A row's key is `(start, end, kind)`, except that several `decl` and
 * `receiver_type` rows share one: TypeScript `getStart()`/`getEnd()` of the
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
export const POSITION_ROWS_VERSION = 7;

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
 * meets that return. The last bit of the first byte: the flags widened to
 * sixteen bits with {@link FLAG_DECLARED_TYPE} (format 7). Pinned to
 * `FLAG_RUNS_HANDED` in `src/position_rows.rs` by
 * `position-rows-runs-handed.test.ts`.
 */
export const FLAG_RUNS_HANDED = 128;
/**
 * A `receiver_type` row only: the row names the type alias a package
 * declares that the receiver was *declared* as, not a type the value holds
 * ("declared, not held"). The compiler's printed type of a proxy is what the
 * alias expands to (an alias of an alias, an intersection), so a reader cannot
 * get the alias from the value. Pinned to `FLAG_DECLARED_TYPE` in
 * `src/position_rows.rs` by `position-rows-declared-type.test.ts`.
 */
export const FLAG_DECLARED_TYPE = 256;

/** The most `decl` rows one position lists. */
export const DECL_CAP = 16;

/** The kinds {@link kindOf} gives a node; `decl` and `jsx` rows are written beside them. */
export type NodeKind = 'call' | 'member' | 'compare' | 'ident';
export type PositionKind = NodeKind | 'decl' | 'jsx' | 'typed_decl' | 'receiver_type';

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
        if (kind === 'member' && !(row[8] & FLAG_CHECKER_THREW)) {
          rows.push(...this.receiverTypeRows(node, row));
          const declared = this.declaredTypeRow(node);
          if (declared) rows.push(declared);
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

  private row(kind: Exclude<PositionKind, 'decl' | 'typed_decl' | 'receiver_type'>, node: ts.Node, sourceFile: ts.SourceFile): PositionRow {
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
   * The `receiver_type` rows of a `member` row, at the
   * span of the object the member is read on: the declaration of each named
   * class the receiver's type holds, written only where the
   * member is declared in another package than that class (an `on` the class
   * inherits from a base package). The checker's type after
   * `getNonNullableType` is read, so an alias of a class is already the
   * class, and an intersection or union gives one row per named constituent
   * (an anonymous object type has no name). The class's package is the one
   * it is declared in, a workspace package or the checkout's own included;
   * one with no package gives none. Sorted by file, then position; past
   * {@link DECL_CAP} one row with {@link FLAG_TOO_MANY} and nothing else. No
   * type text. A throw writes none: the `member` row carries the flag.
   */
  private receiverTypeRows(node: ts.Node, member: PositionRow): PositionRow[] {
    if (!ts.isPropertyAccessExpression(node) && !ts.isElementAccessExpression(node)) return [];
    const memberPackage = member[6];
    if (member[4] === null) return [];
    try {
      const receiver = node.expression;
      const start = receiver.getStart(node.getSourceFile());
      const end = receiver.getEnd();
      const type = this.checker.getNonNullableType(this.checker.getTypeAtLocation(receiver));
      const constituents = type.isUnionOrIntersection() ? type.types : [type];
      const found = new Map<ts.Declaration, ts.Symbol>();
      for (const constituent of constituents) {
        const held = this.checker.getNonNullableType(constituent);
        const symbol = held.getSymbol();
        if (!symbol || symbol.name.startsWith('__')) continue;
        // Classes only: interface rows (a framework's request and response types) were most of the rows' growth.
        if (!(symbol.flags & ts.SymbolFlags.Class)) continue;
        const declaration = this.typeDeclaration(held);
        if (declaration) found.set(declaration, symbol);
      }
      const rows: { start: number; row: PositionRow }[] = [];
      for (const [declaration, symbol] of found) {
        const file = declaration.getSourceFile();
        const at = declaration.getStart(file);
        const { declFile, declLine, declPackage } = this.located(file, at);
        if (declPackage === null || declPackage === memberPackage) continue;
        const name = this.publishedClassName(symbol, declaration, declPackage);
        if (name === undefined) continue;
        let flags = this.publication(symbol, declaration);
        if (declFile === null) flags |= FLAG_DECLARATION_UNNAMED;
        rows.push({
          start: at,
          row: ['receiver_type', start, end, name, declFile, declLine, declPackage, null, flags],
        });
      }
      if (rows.length > DECL_CAP) {
        return [['receiver_type', start, end, null, null, null, null, null, FLAG_TOO_MANY]];
      }
      rows.sort((a, b) => compareNames(a.row[4], b.row[4]) || a.start - b.start);
      return rows.map(({ row }) => row);
    } catch {
      return [];
    }
  }

  /**
   * The {@link FLAG_DECLARED_TYPE} row of a member read's receiver: the type
   * alias a package declares that the receiver was declared as. Read from
   * the declaration, never the value: the receiver symbol's annotation, else
   * the declared return of the make its initializer calls (or the call the
   * receiver is), walked through the checkout's own aliases to the first
   * package alias (the first constituent of a union or intersection that has
   * one). Only aliases: an interface or class is what the class rows are for.
   * Only where the member read sits on a call's callee chain
   * (`a.b.c()`); a plain property read gives none. At most one row per
   * receiver span, outside {@link DECL_CAP}, and not subject to the
   * same-package skip of the class rows. A throw writes none.
   */
  private declaredTypeRow(node: ts.Node): PositionRow | undefined {
    if (!ts.isPropertyAccessExpression(node) && !ts.isElementAccessExpression(node)) return undefined;
    if (!isOnCalleeChain(node)) return undefined;
    try {
      const receiver = node.expression;
      const typeNode = this.declaredTypeNode(receiver);
      if (!typeNode) return undefined;
      const found = this.packageAlias(typeNode, new Set(), 0);
      if (!found) return undefined;
      const { symbol, declaration, declFile, declLine, declPackage } = found;
      const start = receiver.getStart(node.getSourceFile());
      const flags = this.publication(symbol, declaration) | FLAG_DECLARED_TYPE;
      return ['receiver_type', start, receiver.getEnd(), ts.symbolName(symbol), declFile, declLine, declPackage, null, flags];
    } catch {
      return undefined;
    }
  }

  /** The type node a receiver is declared by: its symbol's annotation, or the declared return of the call it is made by. */
  private declaredTypeNode(receiver: ts.Expression): ts.TypeNode | undefined {
    const expression = skipWrappers(receiver);
    if (ts.isCallExpression(expression)) return this.returnTypeNode(expression);
    const name = ts.isPropertyAccessExpression(expression)
      ? expression.name
      : ts.isIdentifier(expression)
        ? expression
        : undefined;
    let symbol = name ? this.checker.getSymbolAtLocation(name) : undefined;
    if (symbol && symbol.flags & ts.SymbolFlags.Alias) symbol = this.checker.getAliasedSymbol(symbol);
    const declaration = symbol?.valueDeclaration;
    if (
      !declaration ||
      !(
        ts.isVariableDeclaration(declaration) ||
        ts.isPropertyDeclaration(declaration) ||
        ts.isPropertySignature(declaration) ||
        ts.isParameter(declaration)
      )
    ) {
      return undefined;
    }
    if (declaration.type) return declaration.type;
    const initializer = 'initializer' in declaration ? declaration.initializer : undefined;
    if (!initializer || ts.isParameter(declaration)) return undefined;
    const made = skipWrappers(initializer);
    return ts.isCallExpression(made) ? this.returnTypeNode(made) : undefined;
  }

  /** The declared return type node of the signature a call resolves to. */
  private returnTypeNode(call: ts.CallExpression): ts.TypeNode | undefined {
    const type = this.checker.getResolvedSignature(call)?.declaration?.type;
    return type && ts.isTypeNode(type) ? type : undefined;
  }

  /**
   * The first type alias a package declares that `typeNode` names, through
   * the checkout's own aliases (`type Runner = Inner`), a union or an
   * intersection, in order. An alias, interface or class of the checkout
   * that is not an alias is a stop; so is the default library.
   */
  private packageAlias(
    typeNode: ts.TypeNode,
    seen: Set<ts.Symbol>,
    depth: number
  ):
    | { symbol: ts.Symbol; declaration: ts.Declaration; declFile: string; declLine: number; declPackage: string }
    | undefined {
    if (depth > DECLARED_WALK_DEPTH) return undefined;
    if (ts.isParenthesizedTypeNode(typeNode)) return this.packageAlias(typeNode.type, seen, depth + 1);
    if (ts.isUnionTypeNode(typeNode) || ts.isIntersectionTypeNode(typeNode)) {
      for (const member of typeNode.types) {
        const found = this.packageAlias(member, seen, depth + 1);
        if (found) return found;
      }
      return undefined;
    }
    if (!ts.isTypeReferenceNode(typeNode)) return undefined;
    const name = ts.isQualifiedName(typeNode.typeName) ? typeNode.typeName.right : typeNode.typeName;
    let symbol = this.checker.getSymbolAtLocation(name);
    if (symbol && symbol.flags & ts.SymbolFlags.Alias) symbol = this.checker.getAliasedSymbol(symbol);
    if (!symbol || seen.has(symbol) || !(symbol.flags & ts.SymbolFlags.TypeAlias)) return undefined;
    seen.add(symbol);
    const declaration = symbol.declarations?.find(ts.isTypeAliasDeclaration);
    if (!declaration) return undefined;
    const file = declaration.getSourceFile();
    const { declFile, declLine, declPackage } = this.located(file, declaration.getStart(file));
    if (declFile === null || declFile.startsWith('lib:')) return undefined;
    if (declFile.startsWith('pkg:') || declFile.split('/').includes('node_modules')) {
      return declPackage === null ? undefined : { symbol, declaration, declFile, declLine, declPackage };
    }
    return this.packageAlias(declaration.type, seen, depth + 1);
  }

  /**
   * The name a `receiver_type` row gives a class. A class written
   * `export default class Cursor` has the symbol name `default`, which is
   * its package's `default` only where it is declared in the file the
   * package's name resolves to; in an inner module it is named as written
   * (`Cursor`), the name a package re-exports it under by convention. An
   * anonymous default class of an inner module has no name: no row.
   */
  private publishedClassName(symbol: ts.Symbol, declaration: ts.Declaration, declPackage: string): string | undefined {
    const name = ts.symbolName(symbol);
    if (name !== ts.InternalSymbolName.Default) return name;
    const file = declaration.getSourceFile();
    if (this.packageEntry(declPackage, file) === file.fileName) return name;
    const own = ts.getNameOfDeclaration(declaration);
    return own && ts.isIdentifier(own) ? own.text : undefined;
  }

  private entries = new Map<string, string | undefined>();

  /**
   * The file `pkg` resolves to from `from`, a file of that package, so the
   * installed copy `from` is in answers (two versions of one package resolve
   * apart). Cached by package and directory.
   */
  private packageEntry(pkg: string, from: ts.SourceFile): string | undefined {
    const key = `${pkg}\0${from.fileName.slice(0, from.fileName.lastIndexOf('/'))}`;
    if (!this.entries.has(key)) {
      const resolved = ts.resolveModuleName(
        pkg,
        from.fileName,
        this.program.getCompilerOptions(),
        this.resolutionHost()
      ).resolvedModule;
      this.entries.set(key, resolved?.resolvedFileName);
    }
    return this.entries.get(key);
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

/** How far a declared type is walked through the checkout's own aliases. */
const DECLARED_WALK_DEPTH = 8;

/** An expression without the wrappers that do not change what it is: `(x)`, `x!`. */
function skipWrappers(expression: ts.Expression): ts.Expression {
  let current = expression;
  while (ts.isParenthesizedExpression(current) || ts.isNonNullExpression(current)) current = current.expression;
  return current;
}

/** Whether a member read is the callee, or an object on the callee's chain, of a call (`a.b.c()`). */
function isOnCalleeChain(node: ts.Node): boolean {
  let current: ts.Node = node;
  for (;;) {
    const parent: ts.Node | undefined = current.parent;
    if (!parent) return false;
    if (ts.isCallExpression(parent)) return parent.expression === current;
    if (ts.isParenthesizedExpression(parent) || ts.isNonNullExpression(parent)) {
      current = parent;
      continue;
    }
    if (
      (ts.isPropertyAccessExpression(parent) || ts.isElementAccessExpression(parent)) &&
      parent.expression === current
    ) {
      current = parent;
      continue;
    }
    return false;
  }
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
    case 'receiver_type':
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
