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
import { ts } from 'ts-morph';
import type { ProjectLoader } from './project-loader.js';
import type { PositionRowsCounts } from './types.js';
/** The format's version, written in the header. */
export declare const POSITION_ROWS_VERSION = 7;
/** The type is `any`, declared or inferred. Never set with the error type. */
export declare const FLAG_ANY = 1;
/** The type is the compiler's error type: something did not resolve. */
export declare const FLAG_ERROR_TYPE = 2;
/** The checker threw at this node; the row holds nothing else. */
export declare const FLAG_CHECKER_THREW = 4;
/** The declaration is outside the checkout, not the default library and in no installed package. */
export declare const FLAG_DECLARATION_UNNAMED = 8;
/** A `decl` row standing for more than {@link DECL_CAP} rows; it names none. */
export declare const FLAG_TOO_MANY = 16;
/**
 * The declaration is published under the row's symbol name: its module
 * exports it by that name, or it is a global (a script's top-level
 * declaration, or one in `declare global`). A member of a class, interface,
 * type literal or object literal never is (carrick#2508).
 */
export declare const FLAG_PUBLISHED = 32;
/** The declaration is what its module exports as `export =` or `default` (carrick#2508). */
export declare const FLAG_MODULE_DEFAULT = 64;
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
export declare const FLAG_RUNS_HANDED = 128;
/**
 * A `receiver_type` row only: the row names the type alias a package
 * declares that the receiver was *declared* as, not a type the value holds
 * ("declared, not held"). The compiler's printed type of a proxy is what the
 * alias expands to (an alias of an alias, an intersection), so a reader cannot
 * get the alias from the value. Pinned to `FLAG_DECLARED_TYPE` in
 * `src/position_rows.rs` by `position-rows-declared-type.test.ts`.
 */
export declare const FLAG_DECLARED_TYPE = 256;
/** The most `decl` rows one position lists. */
export declare const DECL_CAP = 16;
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
    number
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
/**
 * Walk `files`, write their rows to `outPath` and say what was written.
 *
 * The file is written beside `outPath` as `<outPath>.partial` and renamed
 * over it, so a walk that dies leaves no half-written rows file. Every write
 * goes through a {@link WriteGuard} over the file's directory, which refuses
 * a directory inside the scanned trees anywhere but beneath `.carrick`.
 */
export declare function writePositionRows(options: PositionRowsOptions): PositionRowsCounts;
/** What a declaration's file is written as in a row's `decl_file`: `lib:<name>` for the default library, else `PathNames`'. */
export declare function declaringFileName(program: ts.Program, paths: PathNames, file: ts.SourceFile): string | null;
/** The installed package directory a resolved file lies in, and its name, as the module graph places it. */
export type PackageDirectoryOf = (fileName: string) => {
    directory: string;
    name: string;
} | undefined;
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
export declare class PathNames {
    private readonly packageDirectoryOf;
    private readonly roots;
    private readonly packageNames;
    /**
     * Answers by declaring file: a service's rows name a few thousand files
     * hundreds of thousands of times, and the module graph's answer is a scan
     * of its packages (on a Deno service it was 40% of the walk).
     */
    private readonly declaringFiles;
    private readonly declaringPackages;
    constructor(scanRoot: string, packageDirectoryOf?: PackageDirectoryOf);
    /** The root `file` lies under, and its path from there; `undefined` outside every root. */
    private inside;
    /** A request file's name: relative to the root, which every file the scanner sends is inside. */
    relative(file: string): string;
    /** What a declaring file is written as, or `null` when nothing names it. */
    declaringFile(fileName: string): string | null;
    private nameDeclaringFile;
    /**
     * The package a declaring file belongs to: the module graph's or the path's
     * answer (`installedPackageOf`), else, for a file inside the root, the
     * `name` of the nearest package.json at or above it, up to the root.
     */
    declaringPackage(fileName: string, packageOf: (filePath: string) => string | undefined): string | null;
    private nameDeclaringPackage;
    private packageNameIn;
    /**
     * `import("/abs/path")` in a printed type, written with the declaring
     * file's name instead. The print has no enclosing node, so it names every
     * module by the file that declares it and one type reads the same from
     * every file; it also skips the accessibility search an enclosing node
     * costs (40% of the walk on a 600-file service).
     */
    scrubType(text: string): string;
}
/**
 * The rows of one program's files. The `implementations` action
 * (`implementations.ts`, carrick#2432) reads a call's symbol and names its
 * answers through the same walker, so they join as the rows do.
 */
export declare class PositionWalker {
    private readonly program;
    private readonly paths;
    private readonly loader;
    private readonly checker;
    /** Each module's exports by name, and its `export =` and `default` targets, aliases followed. */
    private readonly moduleExports;
    constructor(program: ts.Program, paths: PathNames, loader: ProjectLoader);
    rowsOf(sourceFile: ts.SourceFile): PositionRow[];
    private row;
    /**
     * Whether a call's declared return is the result of the function it is
     * handed (carrick#2634), read from the declared signature through public
     * checker calls. See {@link FLAG_RUNS_HANDED}.
     */
    private runsHanded;
    /** The call signatures of a parameter's type, a type parameter read through its constraint, `null` and `undefined` skipped. */
    private callSignaturesOf;
    /** The compiler's own awaited type differs from the type: `Promise<R>`, `PromiseLike<R>`. */
    private isAwaitable;
    /**
     * An intrinsic element's type: the attributes `JSX.IntrinsicElements`
     * gives its tag. The compiler types an intrinsic tag name itself as the
     * error type, though the name resolves. A tag is intrinsic when it is an
     * identifier that starts with a lowercase letter or holds a `-`, as the
     * language reads it.
     */
    private intrinsicProps;
    /** How rows name a position in a program file: its file, 1-based line and package. */
    located(declaringFile: ts.SourceFile, position: number): {
        declFile: string | null;
        declLine: number;
        declPackage: string | null;
    };
    /**
     * The declaration a member with none is attributed to (carrick#2448): the
     * mapped type node, or the alias naming it, of the receiver's type. A
     * mapped type's members are made by the checker and have no declaration of
     * their own. This is the owner `memberTypeWithoutDeclaration` reads such a
     * member at (type-structural-expander.ts): the type's symbol, else its
     * alias symbol. A union or intersection receiver contributes the one
     * constituent that has the property; with several the row is left as it is.
     */
    private ownerDeclaration;
    /** The declaration of a type's symbol, else of its alias symbol: a mapped type node, or the alias naming it. */
    private typeDeclaration;
    /**
     * How `declaration`'s module publishes `symbol`: {@link FLAG_PUBLISHED}
     * when under the symbol's own name (or as a global), and
     * {@link FLAG_MODULE_DEFAULT} when as the module's `export =` or `default`.
     * The module is the nearest ambient `declare module "…"` or the file.
     */
    private publication;
    /** A namespace member, found among the exports of the module it sits in. */
    private publishedByExports;
    private publishedIn;
    /** The type of the object a `member` or `call` row's name is read on, if the name is read on one. */
    private receiverOf;
    /**
     * The `decl` rows of a `call` or `member` row (spans unset): one per
     * declaration the receiver's union constituents give the member, when they
     * give more than one (carrick#2448). A constituent's declaration is its
     * member's ({@link attributedDeclaration}), or, for a member with none, the
     * constituent type's own ({@link typeDeclaration}). Two instantiations of
     * one generic class, or one class and `undefined`, give one, and no row.
     * A throw writes one row flagged {@link FLAG_CHECKER_THREW}.
     */
    private declRows;
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
    private receiverTypeRows;
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
    private declaredTypeRow;
    /** The type node a receiver is declared by: its symbol's annotation, or the declared return of the call it is made by. */
    private declaredTypeNode;
    /** The declared return type node of the signature a call resolves to. */
    private returnTypeNode;
    /**
     * The first type alias a package declares that `typeNode` names, through
     * the checkout's own aliases (`type Runner = Inner`), a union or an
     * intersection, in order. An alias, interface or class of the checkout
     * that is not an alias is a stop; so is the default library.
     */
    private packageAlias;
    /**
     * The name a `receiver_type` row gives a class. A class written
     * `export default class Cursor` has the symbol name `default`, which is
     * its package's `default` only where it is declared in the file the
     * package's name resolves to; in an inner module it is named as written
     * (`Cursor`), the name a package re-exports it under by convention. An
     * anonymous default class of an inner module has no name: no row.
     */
    private publishedClassName;
    private entries;
    /**
     * The file `pkg` resolves to from `from`, a file of that package, so the
     * installed copy `from` is in answers (two versions of one package resolve
     * apart). Cached by package and directory.
     */
    private packageEntry;
    /**
     * The `typed_decl` row of a call (carrick#2448): the callee is a value
     * (a parameter, a property, a local) whose type is a named function's own
     * (`send: typeof fetch`), so the compiler gives the type a symbol whose
     * declaration is that function's. Written only when the callee symbol is
     * not already that declaration and the type's symbol is named (an
     * anonymous function or type literal is `__function` / `__type`), so a
     * direct call or a method call writes none.
     */
    private typedDeclRow;
    /** The row's symbol, following aliases to a target that has a declaration. */
    symbolOf(kind: PositionKind, node: ts.Node): ts.Symbol | undefined;
    /**
     * In a TypeScript file `const { a } = require("m")` binds `a` as `any` with
     * the binding as its only declaration (TypeScript resolves `require` itself
     * only in JavaScript files), so a trace stops there. Name the export the
     * call resolves to instead (carrick#2454): for the binding itself, and for
     * `m.x` where `m` is a whole-module binding. `undefined` when the name is
     * not such a binding, the specifier is not a literal or does not resolve,
     * or the module has no such export.
     */
    private requiredExport;
    /** The `require` binding a symbol is, when its only declaration is one. */
    private requireBinding;
    /** `"m"` of a call `require("m")` to the global `require`, else `undefined`. */
    private requireSpecifier;
    private exportOfRequired;
    private host;
    private resolutionHost;
}
/** Order two `decl_file` names, `null` last. */
export declare function compareNames(a: string | null, b: string | null): number;
/** The kind of row a node gives, if any (exported for the editor-parity test, carrick#2418). */
export declare function kindOf(node: ts.Node): NodeKind | undefined;
