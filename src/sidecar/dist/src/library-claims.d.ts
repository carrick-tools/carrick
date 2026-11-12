/**
 * `verify_library_claims` (carrick#1616) and `verify_client_semantics`
 * (carrick#1564): check what a model said about a library against the
 * package's own type declarations.
 *
 * A claim says what one export of a package does (its role, from a closed
 * list) and where each part of a call through it sits: which member makes an
 * instance and which option keys it reads, which member acts on the wire,
 * which argument or key holds the name (a path, topic or event), the payload,
 * the handler. The scanner reads call sites through a claim only when this
 * check verified it, and a verified claim becomes a fact that can fail a pull
 * request check. So a claim is `verified` only when the declarations say so
 * positively. `failed` means the declarations resolved and contradict the
 * claim; `unchecked` means they could not be read, or say nothing at the place
 * the claim needs (`any`, `unknown`, `{}`, an unconstrained type parameter).
 * The scanner drops both.
 *
 * One probe file, one set of resolution rules and one set of definitions
 * ("declared", "accepts string", "says nothing") serve every role. The role
 * alone picks which checks a claim needs (`ROLE_TABLE`): `http_client` keeps
 * the #1564 checks and reasons exactly, so `verify_client_semantics` converts
 * its checks into the shared shape (`httpCheck`) and answers through here;
 * `broker`, `in_process_bus` and `socket` take the message checks, which add
 * the contract's must-not-verify rules (a member only another package
 * declares, a name slot beside a string slot the claim does not account for,
 * a handler that says nothing). The wire is pinned on carrick#1564 (comment
 * 5937606126, section 3), as contract amendment 2 (comment 5939543981)
 * changes it: a maker's parts are slots, `of` is a receiver id, and a
 * receiver a generic maker or scope builds is read at its declared
 * type-parameter defaults.
 *
 * The export's type is read the way the service's own code would read it: a
 * probe file in `from_dir` imports it, inside the service's program, under the
 * service's compiler options. The declarations must be the installed
 * package's: a `paths` alias or a `declare module` block the service writes
 * for itself is not evidence about the library.
 */
import { ts, type Project } from 'ts-morph';
import { type PackageDirectoryOf } from './position-rows.js';
import type { ClaimSlot, LibraryCheck, LibrarySurface, ResultShape, SemanticsCheck, SemanticsModule, SemanticsDeclaration, SemanticsResult } from './types.js';
/** How one check came out, before it is stamped with its claim and receiver. */
type Outcome = {
    verdict: 'verified';
    declarations?: DeclaredAt[];
    /**
     * In-process only, never stamped onto a verdict: the declarations of
     * the member a verified op calls, and, for a receive op whose
     * `request` names a `body`, of the member that path ends on
     * (carrick#2674). Read by `labelledMembers`.
     */
    members?: readonly ts.Declaration[];
    bodyMembers?: readonly ts.Declaration[];
    /**
     * Stamped: why a receive op's `request` was dropped while the op
     * itself verified (carrick#2968). Only `request_member_missing`.
     */
    requestDropped?: string;
    /**
     * In-process only: on a type receiver, the declarations its type stands
     * on (`declaredSymbols`), the key a reader joins a receiver by
     * (carrick#3046).
     */
    receiverTypes?: readonly ts.Declaration[];
} | {
    verdict: 'failed' | 'unchecked';
    reason: string;
};
/** Where a member is declared, spelled as a position row's `decl_file` and `decl_line`. */
type DeclaredAt = SemanticsDeclaration;
export interface VerifyResult {
    semantics: SemanticsResult[];
    modules: SemanticsModule[];
}
/**
 * A #1564 HTTP check in the shared shape. The conversion is one to one, so
 * each claim keeps its own id and its own verdict:
 * - `factory` is a `make` call of `member` whose base is the base key of the
 *   options at argument 0;
 * - `verb` is a `request` op with a fixed method and the path at argument 0;
 * - `verb_body` is the same op with the body at argument 1, or with an
 *   options object at argument 1 and the body as one of its keys;
 * - `request` is a `request` op whose url and method keys sit on the config
 *   at argument 0, or whose path is argument 0 and method key sits on the
 *   options at argument 1; `request_body` adds the body key there.
 */
export declare function httpCheck(check: SemanticsCheck): LibraryCheck;
/**
 * What a project's module graph says, where it resolved through one rather
 * than through `node_modules` (carrick#2599). Such a resolver serves a package
 * from a cache whose path has no `node_modules` segment, so neither the path
 * nor a Node resolution of the package name finds the files the program
 * loaded.
 */
export interface ModuleGraph {
    /** The installed package directory a file lies in, and its name. */
    packageDirectoryOf: PackageDirectoryOf;
    /** How the program resolves `name` from `from`; `undefined` where there is no graph, and Node resolution answers. */
    resolveModule(name: string, from: string, options: ts.CompilerOptions): ts.ResolvedModuleFull | undefined;
}
/**
 * Test instrumentation, never on the wire: how many times the lister built a
 * handler parameter's members and printed a type, since the caller last
 * reset them (carrick#2977).
 */
export declare const surfaceReadCounts: {
    handlerMemberBuilds: number;
    typePrints: number;
};
export declare class LibraryClaimsVerifier {
    private readonly project;
    private readonly graph;
    constructor(project: Project, graph?: ModuleGraph);
    /**
     * Judge every check, in request order, spending at most `budgetMs`. Checks
     * the budget does not reach come back `unchecked` with reason `budget`.
     * A check on an instance or a scope reads the maker or scope claims of the
     * same request first, whatever their place in it.
     */
    run(fromDir: string, checks: LibraryCheck[], budgetMs: number, scanRoot?: string, options?: RunOptions): VerifyResult;
    /**
     * The members a service's server-framework labels name, read with the
     * verifier itself so only a label that verifies in the service's own
     * program counts (carrick#2674): each verified `send` op that carries a
     * `payload` slot, and each verified `receive` op whose `request` names a
     * `body` path. Other checks are not judged.
     */
    labelledMembers(fromDir: string, checks: LibraryCheck[], budgetMs: number, scanRoot?: string, typePackages?: Readonly<Record<string, readonly string[]>>): LabelledMembers;
    /**
     * Each specifier's declared surface, read with the verifier's predicates
     * (carrick#1660): every value export (or only those `only` names for the
     * specifier), the receivers a claim can be read on, in the verifier's
     * receiver grammar, and each receiver's callable members with their
     * parameter slots. A runtime module (`node:events`) is listed from the
     * runtime's types package, as the message roles read it. Capped at
     * `maxEntries` per specifier, with the count dropped: every export and
     * receiver is listed before any member name, and every member name before
     * any signature, so the cap cuts signatures first and exports last.
     *
     * A receiver a maker builds is listed as the verifier reads it: built with
     * no argument and no type argument, so a generic maker's instance is read
     * at its declared type-parameter defaults (carrick#1696). That takes two
     * programs. The first reads which receivers each export can make; the
     * second adds the verifier's own build statement for each
     * (`receiverExpression` on the export's `importLine`, read through
     * `DeclarationReader.returnOf`) and lists from it.
     *
     * A member of an `instance:` receiver whose call returns another object
     * that routes are registered on lists that object as its `scope_receiver`
     * (carrick#2774), built as the verifier builds a scope step, under a cap of
     * its own and left out of the hash.
     *
     * Each member's handler-parameter members (carrick#2674 S3) are listed
     * only when `options.handlers` is set, and each signature's result
     * candidates (carrick#2817) only when `options.resultCandidates` is
     * (carrick#2966): on a large package either costs more than the rest of the
     * listing.
     *
     * `surface_sha256` is the full-surface hash (see `fullSurfaceSha256`).
     */
    listSurface(fromDir: string, packages: string[], maxEntries: number, only?: Readonly<Record<string, string[]>>, options?: SurfaceOptions): ListedSurface;
    /** `read` over a probe file holding `lines` in `fromDir`, removed again afterwards. */
    private readProbe;
}
/** The answer to `list_library_surface`. */
export interface ListedSurface {
    surfaces: LibrarySurface[];
    surface_sha256: string;
}
/**
 * The full-surface hash the shared library store keys on: sha256 of the JSON
 * array of `[package, exports]` for every surface that listed at least one
 * export, sorted by `package` (code unit order). A specifier that lists
 * nothing (unresolved, local, no value exports) is left out, so asking for a
 * subpath a version does not have changes nothing. Handler members, result
 * candidates and scope receivers are not hashed. Type text already names
 * the request's directory as `<root>`.
 */
export declare function fullSurfaceSha256(surfaces: readonly LibrarySurface[]): string;
/** What `run` reads beyond the checks; none of it is part of a verdict. */
interface RunOptions {
    /** Called with each check's outcome, in request order. */
    observe?: (check: LibraryCheck, outcome: Outcome) => void;
    /** Read server `send` ops as replies (`SERVER_REPLY_RULES`). */
    readReplies?: boolean;
    /**
     * Per package specifier, the type-only packages the store says its surface
     * re-exports (carrick#2674): a member one of them declares is the
     * package's own, as the store reads it.
     */
    typePackages?: Readonly<Record<string, readonly string[]>>;
}
/**
 * A declaration's key within one process: its file as the compiler names it
 * and its start. Two programs of one project name a declaration alike, so a
 * label read in the verifier's probe program joins a member read in the
 * inferrer's (carrick#2674).
 */
export declare function declarationKey(declaration: ts.Node): string;
/**
 * The members a service's verified server-framework labels name, by
 * {@link declarationKey} (carrick#2674): each `send` op member with the slot
 * its payload sits in, and each member a receive op's `request.body` path
 * ends on.
 */
export interface LabelledMembers {
    sends: ReadonlyMap<string, ClaimSlot>;
    /**
     * The properties a service assigns to reply (`form: set`, carrick#3047),
     * by {@link declarationKey} of the property, with the slot the assigned
     * value sits in (always argument 0). Never in `sends`.
     */
    sets: ReadonlyMap<string, ClaimSlot>;
    /**
     * The route members whose handler's return is the reply (`returns` on a
     * receive op, carrick#3054), by {@link declarationKey} of the route member,
     * with the slot the handler sits in and the part of the return that holds
     * the body. Never in `sends`.
     */
    returns: ReadonlyMap<string, {
        handler: ClaimSlot;
        result?: ResultShape;
    }>;
    bodies: ReadonlySet<string>;
    /** How each label read came out (`verified`, or the reason it was not), for the log. */
    outcomes?: Readonly<Record<string, number>>;
}
/** What a surface listing adds beyond the hashed surface, each only when asked (carrick#2966). */
export interface SurfaceOptions {
    /** Each member's `handlers` (carrick#2674 S3). */
    handlers?: boolean;
    /** `types[]` widened, `settable`, `types_left_out` (carrick#3050). */
    types?: boolean;
    /** Each signature's `result_candidates` (carrick#2817). */
    resultCandidates?: boolean;
    /** Handler-parameter members the whole call may list (default `HANDLER_LISTING_BUDGET`, carrick#3016). */
    maxHandlerMembers?: number;
    /** Estimated member makers of foreign values past which they list none (default `FOREIGN_MAKER_BUDGET`, carrick#3745). */
    foreignMakerBudget?: number;
}
/** The name a service imports a package by: `@types/scope__name` is `@scope/name`, `@types/name` is `name`. */
export declare function importedNameOf(installed: string): string;
/** The package a specifier names: `@scope/name` or `name`, without a subpath. */
export declare function packageNameOf(specifier: string): string;
export {};
