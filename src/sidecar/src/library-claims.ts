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

import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { ts, type Project } from 'ts-morph';
import { holdsResult, resultCandidatesOf } from './result-candidates.js';
import { publicNameOf } from './capture/installed-package.js';
import { RUNTIME_EMITTER_EVENTS, RUNTIME_TYPE_PACKAGES, isRuntimeEmitterMember } from './runtime-emitter.js';
import { PathNames, declaringFileName, type PackageDirectoryOf } from './position-rows.js';
import type {
  ClaimSlot,
  KeyLabel,
  LibraryCheck,
  LibraryClaim,
  LibraryRole,
  LibrarySurface,
  RequestAt,
  ResultShape,
  RunsSlot,
  SemanticsCheck,
  SemanticsModule,
  SemanticsDeclaration,
  SemanticsResult,
  SurfaceExport,
  SurfaceHandler,
  SurfaceHandlerMember,
  SurfaceHandlerParam,
  SurfaceHandlerType,
  SurfaceKey,
  SurfaceMember,
  SurfaceParam,
  SurfaceReceiver,
  SurfaceSignature,
  SurfaceSettable,
  SurfaceTypeExport,
} from './types.js';
import { memberDeclaration } from './declaration-attribution.js';

/** The methods a `verb` claim may name, and a method key may accept. */
const HTTP_METHODS: ReadonlySet<string> = new Set([
  'GET',
  'POST',
  'PUT',
  'PATCH',
  'DELETE',
  'HEAD',
  'OPTIONS',
]);

/** The methods a route may register: every request method, and `ALL` for a member that registers each. */
const ROUTE_METHODS: ReadonlySet<string> = new Set([...HTTP_METHODS, 'ALL']);

/**
 * Interfaces whose members every value of that kind inherits. A key found only
 * on one of these (`constructor`, `toString`, a string's `length`) is not a
 * key the library declares.
 */
const BUILTIN_INTERFACES: ReadonlySet<string> = new Set([
  'Object',
  'Function',
  'CallableFunction',
  'NewableFunction',
  'String',
  'Number',
  'Boolean',
  'Symbol',
  'BigInt',
  'Array',
  'ReadonlyArray',
]);

/**
 * A runtime module's own specifier (`node:events`). Its declarations are the
 * runtime's types package's, which is therefore its home (contract
 * amendment 1, A1, on carrick#1564). The bare name (`events`) is a registry
 * package's, never the runtime module.
 */
const RUNTIME_MODULE = /^node:/;

/**
 * A key for the language's own library (carrick#2445): `lib:<lib file>`
 * (`lib:lib.dom.d.ts`) names the globals that default-library file declares.
 * Its installed version is the compiler's, so the store keys it
 * `lib:lib.dom.d.ts@<typescript version>`. A global has no specifier: the
 * probe reads it as `globalThis.<name>`, and its home is the default-library
 * file set (`DEFAULT_LIBRARY_HOME`).
 */
const DEFAULT_LIBRARY = /^lib:/;

/**
 * The owner a home names for every default-library file. No package name
 * holds a `:`, so only a `lib:` key's home ever holds it.
 */
const DEFAULT_LIBRARY_HOME = 'lib:';

/** A dependency range that names the service's own source, not a registry release. */
const LOCAL_RANGE = /^(workspace|file|link|portal):/;

/** A resolved module lands on TypeScript: a declaration file or source. */
const TYPESCRIPT_FILE = /\.(d\.[mc]?ts|[mc]?tsx?)$/;

const IDENTIFIER = /^[A-Za-z_$][\w$]*$/;

/** The HTTP receivers of #1564: the export, or what one factory member returns. */
const HTTP_RECEIVER = /^(export|instance:\S+)$/;

type MakeClaim = Extract<LibraryClaim, { kind: 'make' }>;
type ScopeClaim = Extract<LibraryClaim, { kind: 'scope' }>;
type OpClaim = Extract<LibraryClaim, { kind: 'op' }>;
type ReservedClaim = Extract<LibraryClaim, { kind: 'reserved' }>;
type RequestArgs = 'config' | 'path_options';
type KeyLabels = Readonly<Record<string, KeyLabel>>;

/** Which checks a role's claims need. */
interface RoleRules {
  /** The #1564 HTTP checks, with their reasons. */
  http: boolean;
  /**
   * The message checks: a member must be one the receiver's home packages
   * declare (not only another package's base), a name slot must be the only
   * string slot of its call the claim does not account for (design D2) and
   * not a key of an index-signature map, a handler must declare a signature,
   * an instance must come from a maker claim that holds, and a workspace or
   * `file:` dependency is not checked.
   */
  message: boolean;
  /** The op kinds it can check. */
  ops: ReadonlySet<string>;
  /**
   * Label structure v2 (carrick#2434): the method words an op's `method`,
   * `method_key` and `method_default` may name. Without it, an op carrying
   * any of them is `claim_invalid`.
   */
  methods?: ReadonlySet<string>;
  /**
   * A `send` op needs no name: it answers on the connection the request
   * came in on (carrick#2674).
   */
  namelessSends?: boolean;
}

const HTTP_RULES: RoleRules = { http: true, message: false, ops: new Set(['request']) };
const MESSAGE_RULES: RoleRules = {
  http: false,
  message: true,
  ops: new Set(['send', 'receive', 'request']),
};
/**
 * An `http_client` export whose claims in the request use a label v2 field
 * (`hasV2Field`) is read as the store's other roles are: its makers, `of`
 * and `on`, paths and the name-slot rules, plus the method fields. An export
 * whose claims use none keeps the #1564 checks exactly (`HTTP_RULES`).
 */
const HTTP_V2_RULES: RoleRules = { http: false, message: true, ops: new Set(['request']), methods: HTTP_METHODS };
/**
 * A server framework's route members: a `receive` op whose name is the path,
 * the method from the member's own name (`get` registers GET) or from a slot.
 */
const SERVER_RULES: RoleRules = { http: false, message: true, ops: new Set(['receive']), methods: ROUTE_METHODS };

/**
 * A server's rules as `labelledMembers` reads them (carrick#2674): a `send`
 * op is the reply a handler sends back through its reply parameter, on the
 * connection the request came in on, so it names nothing on the wire. Only
 * the stated-parameter reading takes these; `run` keeps `SERVER_RULES`, so
 * no verdict the scanner reads rows from changes.
 */
const SERVER_REPLY_RULES: RoleRules = { ...SERVER_RULES, ops: new Set(['receive', 'send']), namelessSends: true };

/**
 * The role table: the role alone picks the checks. A role this build does
 * not read yet (GraphQL executors) answers `role_unsupported`, and `none`
 * has nothing on the wire to check. A server's mounts are scope claims
 * (`mounts`, #2474 R0).
 */
const ROLE_TABLE: Readonly<Record<LibraryRole, RoleRules | undefined>> = {
  http_client: HTTP_RULES,
  broker: MESSAGE_RULES,
  in_process_bus: MESSAGE_RULES,
  socket: MESSAGE_RULES,
  graphql_client: undefined,
  server_framework: SERVER_RULES,
  none: undefined,
};

/**
 * Whether a claim uses a field label structure v2 added (carrick#2434). An
 * `http_client` export with one such claim in the request is read under
 * `HTTP_V2_RULES`.
 */
function hasV2Field(claim: LibraryClaim): boolean {
  if (claim.kind === 'make') return claim.runs !== undefined;
  if (claim.kind !== 'op') return false;
  return claim.method_default !== undefined || claim.base !== undefined || claim.runs !== undefined || claim.yields !== undefined;
}

/** How one check came out, before it is stamped with its claim and receiver. */
type Outcome =
  | {
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
    }
  | { verdict: 'failed' | 'unchecked'; reason: string };

/** Where a member is declared, spelled as a position row's `decl_file` and `decl_line`. */
type DeclaredAt = SemanticsDeclaration;

const VERIFIED: Outcome = { verdict: 'verified' };
const failed = (reason: string): Outcome => ({ verdict: 'failed', reason });
const unchecked = (reason: string): Outcome => ({ verdict: 'unchecked', reason });

/**
 * A rest parameter typed by a type variable (`...rest: A`): the element at a
 * position is `A[number]`, which says nothing the predicates can read.
 */
const VARIADIC = Symbol('variadic element');

/** What a signature has at one parameter position. */
type Slot = ts.Type | typeof VARIADIC;

/**
 * A failure some signature reached, ranked by how far into the predicate it
 * got. When no signature satisfies a claim, the verdict reports the one that
 * got furthest, so a wrong key reads `key_missing` rather than the
 * `param_missing` of an unrelated overload. At the same depth, `unchecked`
 * wins: an overload whose types say nothing there might have satisfied it.
 */
interface RankedFailure {
  rank: number;
  outcome: Outcome;
}

function furthest(current: RankedFailure | undefined, rank: number, outcome: Outcome): RankedFailure {
  if (!current || rank > current.rank) return { rank, outcome };
  if (rank === current.rank && current.outcome.verdict === 'failed' && outcome.verdict === 'unchecked') {
    return { rank, outcome };
  }
  return current;
}

/** A read that either produced a value or a reason it could not. */
type Read<T> = { value: T } | { reason: string };

/** The callee's signatures, or the verdict when there is nothing to call. */
/** A callee's signatures, and the type its member was read from. */
/** A member a path reaches and the type it is read on. */
interface NamedMember {
  property: ts.Symbol;
  holder: ts.Type;
}

/** `named` is unset when the callee is the receiver itself. */
/** What a `runs` slot must hold: undefined when it holds. */
type RunsPredicate = (declared: Slot | undefined) => Outcome | undefined;
type Callable = { signatures: readonly ts.Signature[]; holder: ts.Type; named?: NamedMember } | { failure: Outcome };

interface ModuleRead {
  entry: SemanticsModule;
  /**
   * A `node:` key that names a file of the runtime's types package, not a
   * module (carrick#2445): the file whose globals the key's exports are.
   */
  globalFile?: ts.SourceFile;
  /** The reason every check on this package is unchecked, when there is one. */
  reason?: string;
}

interface ExportRead {
  type: ts.Type;
  /** The export's own declaration, at the end of its re-export chain. */
  target: ts.Symbol;
}

/**
 * An object a message claim's member is read on, and the packages whose
 * declarations are its own: the named package and its types package, the
 * packages its export is re-exported from, and the package that declares its
 * type (see `DeclarationReader.homeOf`).
 */
interface Holder {
  type: ts.Type;
  home: ReadonlySet<string>;
}

/** A receiver a message claim is read on. */
interface MessageReceiver extends Holder {
  /** The specifier the claim names. */
  pkg: string;
  /** A maker claim for this instance binds a name (`{ bound: 'maker' }` reads it). */
  makerBindsName: boolean;
  /** The receiver came through a scope member (`{ bound: 'scope' }` reads it). */
  scoped: boolean;
  /** The maker overloads this instance is read through: what `runs: { bound: 'maker' }` reads. */
  makerHolding?: readonly ts.Signature[];
  /**
   * The request holds a verified `collects` scope of this package
   * (`{ bound: 'key' }` reads it, #2474 R6a). Read only for an op that names
   * its key.
   */
  keyBound?: boolean;
  /**
   * The export whose own call or construction made this instance
   * (`instance:()`, `instance:new`, with no scope step): a `yields` naming it
   * names this instance (carrick#2663).
   */
  madeBy?: string;
}

/** A check's outcome, and for a maker or scope claim the overloads it holds on. */
interface Judged {
  outcome: Outcome;
  holding?: readonly ts.Signature[];
}

/** A receiver string, parsed: the instance step and the scope step. */
interface ReceiverPath {
  base: string;
  maker?: { form: 'call' | 'new'; member: string | null };
  /** The scope member, after its path, joined by `.` (`tasks.channel`). */
  scope?: string;
  /** The export is a type, read at its defaults (`type`, carrick#2977): no maker, no scope step. */
  type?: true;
}

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
export function httpCheck(check: SemanticsCheck): LibraryCheck {
  const common = {
    claim_id: check.claim_id,
    package: check.package,
    export: check.export,
    role: 'http_client' as const,
    receiver: check.receiver,
  };
  const claim = check.claim;
  switch (claim.kind) {
    case 'factory':
      return {
        ...common,
        claim: { kind: 'make', form: 'call', member: claim.member, base: { arg: 0, key: claim.base_url_key } },
      };
    case 'verb':
      return {
        ...common,
        claim: { kind: 'op', op: 'request', member: claim.member, method: claim.method, name: { arg: 0 } },
      };
    case 'verb_body':
      return {
        ...common,
        claim:
          claim.args === 'path_body'
            ? { kind: 'op', op: 'request', member: claim.member, name: { arg: 0 }, payload: { arg: 1 } }
            : {
                kind: 'op',
                op: 'request',
                member: claim.member,
                name: { arg: 0 },
                options: { arg: 1 },
                ...(claim.body_key === undefined ? {} : { payload: { arg: 1, key: claim.body_key } }),
              },
      };
    case 'request':
    case 'request_body': {
      const at = claim.args === 'config' ? 0 : 1;
      const name: ClaimSlot | undefined =
        claim.args === 'config'
          ? claim.url_key === undefined
            ? undefined
            : { arg: 0, key: claim.url_key }
          : { arg: 0 };
      return {
        ...common,
        claim: {
          kind: 'op',
          op: 'request',
          member: claim.member,
          ...(name === undefined ? {} : { name }),
          method_key: { arg: at, key: claim.method_key },
          ...(claim.kind === 'request_body' ? { payload: { arg: at, key: claim.body_key } } : {}),
        },
      };
    }
  }
}

/** Declarations in a fixed order, each once. */
function sortedDeclarations(declarations: readonly DeclaredAt[]): DeclaredAt[] {
  const seen = new Map<string, DeclaredAt>();
  for (const each of declarations) seen.set(`${each.decl_file}:${each.decl_line}`, each);
  return [...seen.values()].sort((a, b) => (a.decl_file < b.decl_file ? -1 : a.decl_file > b.decl_file ? 1 : a.decl_line - b.decl_line));
}

let probeSequence = 0;

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

const NO_MODULE_GRAPH: ModuleGraph = { packageDirectoryOf: () => undefined, resolveModule: () => undefined };

/**
 * Test instrumentation, never on the wire: how many times the lister built a
 * handler parameter's members and printed a type, since the caller last
 * reset them (carrick#2977).
 */
export const surfaceReadCounts = { handlerMemberBuilds: 0, typePrints: 0 };

export class LibraryClaimsVerifier {
  constructor(
    private readonly project: Project,
    private readonly graph: ModuleGraph = NO_MODULE_GRAPH
  ) {}

  /**
   * Judge every check, in request order, spending at most `budgetMs`. Checks
   * the budget does not reach come back `unchecked` with reason `budget`.
   * A check on an instance or a scope reads the maker or scope claims of the
   * same request first, whatever their place in it.
   */
  run(
    fromDir: string,
    checks: LibraryCheck[],
    budgetMs: number,
    scanRoot?: string,
    options: RunOptions = {}
  ): VerifyResult {
    const { observe, readReplies = false, typePackages = {} } = options;
    const deadline = performance.now() + budgetMs;
    const stamp = (check: LibraryCheck, outcome: Outcome): SemanticsResult =>
      outcome.verdict === 'verified'
        ? {
            claim_id: check.claim_id,
            receiver: check.receiver,
            verdict: 'verified',
            ...(outcome.declarations?.length ? { declarations: outcome.declarations } : {}),
            ...(outcome.requestDropped === undefined ? {} : { request_dropped: outcome.requestDropped }),
          }
        : {
            claim_id: check.claim_id,
            receiver: check.receiver,
            verdict: outcome.verdict,
            reason: outcome.reason,
          };

    if (checks.length === 0) return { semantics: [], modules: [] };

    // One import line per distinct (package, export), in first-seen order.
    const importKeys: string[] = [];
    const importIndex = new Map<string, number>();
    // HTTP: the base-URL keys the request's factory claims name, per factory:
    // an instance is read through the overload that declares them.
    const factoryKeys = new Map<string, Set<string>>();
    // The roles each (package, export) is given across the request.
    const roles = new Map<string, Set<LibraryRole>>();
    // The `http_client` exports read under the label v2 rules.
    const v2Exports = new Set<string>();
    for (const check of checks) {
      if (check.role === 'http_client' && hasV2Field(check.claim)) v2Exports.add(exportKey(check));
    }
    const rulesFor = (check: LibraryCheck): RoleRules | undefined =>
      check.role === 'http_client' && v2Exports.has(exportKey(check))
        ? HTTP_V2_RULES
        : readReplies && check.role === 'server_framework'
          ? SERVER_REPLY_RULES
          : ROLE_TABLE[check.role];
    for (const check of checks) {
      const key = exportKey(check);
      if (!importIndex.has(key)) {
        importIndex.set(key, importKeys.length);
        importKeys.push(key);
      }
      const given = roles.get(key) ?? new Set<LibraryRole>();
      given.add(check.role);
      roles.set(key, given);
      const claim = check.claim;
      const baseKey = claim.kind === 'make' && claim.base?.arg === 0 ? claim.base.key : undefined;
      if (check.role === 'http_client' && claim.kind === 'make' && claim.member !== null && baseKey !== undefined) {
        const factory = factoryKey(key, claim.member);
        const keys = factoryKeys.get(factory) ?? new Set<string>();
        keys.add(baseKey);
        factoryKeys.set(factory, keys);
      }
    }
    const importLines = importKeys.map((key, i) => {
      const [pkg, name] = JSON.parse(key) as [string, string];
      return probeLine(pkg, name, `__carrick_e${i}`);
    });
    // One statement per receiver a message check's maker or scope claim
    // builds, after the imports: that receiver built with no argument and no
    // type argument. Its resolved signature is the maker (or scope) read at
    // its declared type-parameter defaults (`DeclarationReader.returnOf`). An
    // HTTP request adds none, so its probe is the one #1564 reads.
    const builtLines: string[] = [];
    const builtIndex = new Map<string, number>();
    for (const check of checks) {
      if (!rulesFor(check)?.message) continue;
      const built = builtReceiver(check);
      if (built === undefined) continue;
      const key = builtKey(check, built);
      if (builtIndex.has(key)) continue;
      const expression = receiverExpression(`__carrick_e${importIndex.get(exportKey(check))!}`, built);
      if (expression === undefined) continue;
      builtIndex.set(key, importLines.length + builtLines.length);
      builtLines.push(`${expression};`);
    }
    // A check on receiver `type` (carrick#2977) reads its export as a type,
    // declared at its defaults after the export's import, as the lister lists it.
    for (const check of checks) {
      if (!rulesFor(check)?.message || check.receiver !== TYPE_RECEIVER) continue;
      const key = builtKey(check, TYPE_RECEIVER);
      if (builtIndex.has(key)) continue;
      builtIndex.set(key, importLines.length + builtLines.length);
      builtLines.push(typeBuildLine(builtLines.length, `__carrick_e${importIndex.get(exportKey(check))!}`));
    }
    // A `node:` key may name a file of the runtime's types, whose exports are
    // globals (`runtimeGlobalFile`): the probe reads each as `globalThis` holds
    // it, beside its import, and the reader takes the one the key resolves to.
    const globalIndex = new Map<string, number>();
    const globalLines: string[] = [];
    importKeys.forEach((key, i) => {
      const [pkg, name] = JSON.parse(key) as [string, string];
      if (!RUNTIME_MODULE.test(pkg)) return;
      globalIndex.set(key, importLines.length + builtLines.length + globalLines.length);
      globalLines.push(globalRead(name, `__carrick_g${i}`));
    });
    const probeText = [...importLines, ...builtLines, ...globalLines].join('\n');

    const probePath = path.join(fromDir, `__carrick_claims_probe_${process.pid}_${probeSequence++}.ts`);
    const probe = this.project.createSourceFile(probePath, `${probeText}\n`, { overwrite: true });
    try {
      const program = this.project.getProgram().compilerObject;
      const checker = program.getTypeChecker();
      const file = program.getSourceFile(probe.getFilePath());
      if (!file) throw new Error(`probe file ${probePath} is not in the program`);
      const reader = new DeclarationReader(
        program,
        file,
        this.project.getModuleResolutionHost(),
        fromDir,
        scanRoot,
        this.graph
      );
      const localRanges = readLocalRanges(fromDir);

      const moduleReads = new Map<string, ModuleRead>();
      const exportReads = new Map<string, Read<ExportRead>>();
      const httpReceivers = new Map<string, Read<ts.Type>>();
      const judged = new Map<number, Judged>();

      /** The probe's import of the check's export, or its read of a global (`probeLine`). */
      const statementOf = (check: LibraryCheck) => file.statements[importIndex.get(exportKey(check))!];
      const globalStatementOf = (check: LibraryCheck) => file.statements[globalIndex.get(exportKey(check)) ?? -1];

      /** The probe's no-argument build of `receiver` on the check's export, resolved. */
      const atDefaults = (check: LibraryCheck, receiver: string): ts.Signature | undefined => {
        const index = builtIndex.get(builtKey(check, receiver));
        return index === undefined ? undefined : resolvedBuild(checker, file.statements[index]);
      };

      // A runtime module is read for the message roles only: HTTP answers as #1564 does.
      const readModule = (check: LibraryCheck, runtimeModules: boolean): ModuleRead => {
        const key = `${runtimeModules}\u0000${check.package}`;
        let moduleRead = moduleReads.get(key);
        if (!moduleRead) {
          moduleRead = DEFAULT_LIBRARY.test(check.package)
            ? reader.readDefaultLibrary(check.package)
            : reader.readModule(check.package, statementOf(check) as ts.ImportDeclaration, runtimeModules);
          moduleReads.set(key, moduleRead);
        }
        return moduleRead;
      };

      const readExport = (check: LibraryCheck): Read<ExportRead> => {
        const key = exportKey(check);
        let exportRead = exportReads.get(key);
        if (!exportRead) {
          const globalFile = RUNTIME_MODULE.test(check.package) ? readModule(check, rulesFor(check)?.message ?? false).globalFile : undefined;
          exportRead = DEFAULT_LIBRARY.test(check.package)
            ? reader.readGlobal(reader.defaultLibraryFile(check.package), statementOf(check))
            : globalFile
              ? reader.readGlobal(globalFile, globalStatementOf(check))
              : reader.readExport(statementOf(check) as ts.ImportDeclaration);
          exportReads.set(key, exportRead);
        }
        return exportRead;
      };

      /**
       * The message receivers of check `index`, reading its maker and scope
       * claims: one per distinct type the holding overloads return. A maker
       * whose two overloads both hold builds either instance, so a claim on
       * it must hold on each.
       */
      const readMessageReceivers = (index: number, exported: ExportRead): Read<MessageReceiver[]> => {
        const check = checks[index];
        // `fitsReceiver` admitted it.
        const receiverPath = parseReceiver(check.receiver)!;
        let types: readonly ts.Type[] = [exported.type];
        let makerBindsName = false;
        let makerHolding: readonly ts.Signature[] | undefined;
        if (receiverPath.maker) {
          const maker = receiverPath.maker;
          const makers = checks
            .map((other, i) => ({ other, i }))
            .filter(
              ({ other }) =>
                other.package === check.package &&
                other.export === check.export &&
                other.receiver === 'export' &&
                other.claim.kind === 'make' &&
                other.claim.form === maker.form &&
                other.claim.member === maker.member
            );
          const makerJudgements = makers.map(({ i }) => judge(i));
          const made = holdingReturns(makerJudgements, atDefaults(check, receiverPath.base));
          if ('reason' in made) return { reason: made.reason === 'unresolved' ? 'maker_unresolved' : 'maker_unverified' };
          types = made.value;
          makerHolding = makerJudgements.reduce<readonly ts.Signature[]>(
            (common, judgement) => common.filter(sig => judgement.holding!.includes(sig)),
            makerJudgements[0].holding!
          );
          makerBindsName = makers.some(({ other }) => other.claim.kind === 'make' && other.claim.name !== undefined);
        }
        if (receiverPath.scope !== undefined) {
          const scope = receiverPath.scope;
          const scopes = checks
            .map((other, i) => ({ other, i }))
            .filter(
              ({ other }) =>
                other.package === check.package &&
                other.export === check.export &&
                other.receiver === receiverPath.base &&
                other.claim.kind === 'scope' &&
                scopeStep(other.claim) === scope
            );
          const scoped = holdingReturns(
            scopes.map(({ i }) => judge(i)),
            atDefaults(check, check.receiver)
          );
          if ('reason' in scoped) return { reason: scoped.reason === 'unresolved' ? 'scope_unresolved' : 'scope_unverified' };
          types = scoped.value;
        }
        if (types.some(type => reader.returnSaysNothing(type))) {
          return { reason: receiverPath.maker ? 'maker_unresolved' : 'export_untyped' };
        }
        const claim = check.claim;
        const keyBound =
          claim.kind === 'op' && claim.name !== undefined && 'bound' in claim.name && claim.name.bound === 'key'
            ? verifiedCollects(check.package).length > 0
            : undefined;
        return {
          value: types.map(type => ({
            type,
            pkg: check.package,
            home: withTypePackages(
              reader.homeOf(check.package, [exported.target, ...typeSymbols(type)]),
              typePackages[check.package]
            ),
            makerBindsName,
            scoped: receiverPath.scope !== undefined,
            makerHolding,
            ...(keyBound === undefined ? {} : { keyBound }),
            madeBy: receiverPath.maker?.member === null && receiverPath.scope === undefined ? check.export : undefined,
          })),
        };
      };

      /**
       * The one receiver of a check on `type` (carrick#2977): the export read
       * as a type at its declared type-parameter defaults, its home from the
       * declaration the import names, made by nothing. `type_unresolved` when
       * the name has no type meaning or needs a type argument.
       */
      const readTypeReceiver = (check: LibraryCheck): Read<MessageReceiver[]> => {
        const index = builtIndex.get(builtKey(check, TYPE_RECEIVER));
        const built = index === undefined ? undefined : builtType(checker, statementOf(check), file.statements[index]);
        if (built === undefined) return { reason: 'type_unresolved' };
        return {
          value: [
            {
              type: built.type,
              pkg: check.package,
              home: withTypePackages(
                reader.homeOf(check.package, [built.symbol, ...typeSymbols(built.type)]),
                typePackages[check.package]
              ),
              makerBindsName: false,
              scoped: false,
            },
          ],
        };
      };

      /**
       * What a set of maker (or scope) judgements build: the distinct types
       * their common overloads return, each read at its declared
       * type-parameter defaults where `built` (the probe's no-argument build
       * of that receiver) instantiates it.
       */
      const holdingReturns = (judgements: Judged[], built: ts.Signature | undefined): Read<ts.Type[]> => {
        if (judgements.length === 0) return { reason: 'unverified' };
        let holding: readonly ts.Signature[] | undefined;
        for (const judgement of judgements) {
          if (judgement.outcome.verdict !== 'verified' || !judgement.holding) return { reason: 'unverified' };
          holding = holding === undefined ? judgement.holding : holding.filter(sig => judgement.holding!.includes(sig));
        }
        const returns = [...new Set((holding ?? []).map(sig => reader.returnOf(sig, built)))];
        if (returns.length === 0) return { reason: 'unresolved' };
        return { value: returns };
      };

      /**
       * The `collects` scope checks of the request (#2474 R6a) that verify,
       * of one package or of any: judged only when a claim reads them, so a
       * request with none judges exactly what it judged before.
       */
      const verifiedCollects = (pkg?: string): number[] =>
        checks
          .map((other, i) => ({ other, i }))
          .filter(
            ({ other }) =>
              other.claim.kind === 'scope' && other.claim.collects !== undefined && (pkg === undefined || other.package === pkg)
          )
          .filter(({ i }) => judge(i).outcome.verdict === 'verified')
          .map(({ i }) => i);

      /**
       * What the request's verified `collects` scopes return, read at their
       * defaults: the collections an adapter's `runs` slot can take, matched
       * across packages (an HTTP framework's adapter serves another
       * package's router). Undefined when there are none.
       */
      const collections = (): ts.Type[] | undefined => {
        const types = new Set<ts.Type>();
        for (const i of verifiedCollects()) {
          const built = builtReceiver(checks[i]);
          const read = holdingReturns([judge(i)], built === undefined ? undefined : atDefaults(checks[i], built));
          if ('value' in read) read.value.forEach(type => types.add(type));
        }
        return types.size > 0 ? [...types] : undefined;
      };

      const judge = (index: number): Judged => {
        const cached = judged.get(index);
        if (cached) return cached;
        // A maker read on its own instance, or a scope on itself, is a cycle.
        judged.set(index, { outcome: unchecked('receiver_invalid') });
        const result = judgeCheck(index);
        judged.set(index, result);
        return result;
      };

      const judgeCheck = (index: number): Judged => {
        const check = checks[index];
        const rules = rulesFor(check);
        if (!rules) return { outcome: unchecked('role_unsupported') };
        // An export given two roles is classified two ways; neither is a fact.
        if ((roles.get(exportKey(check))?.size ?? 0) > 1) return { outcome: unchecked('role_conflict') };
        if (placementInvalid(check.claim)) return { outcome: unchecked('claim_invalid') };
        if (rules.http && !HTTP_RECEIVER.test(check.receiver)) return { outcome: unchecked('receiver_invalid') };
        if (rules.message && !fitsReceiver(check)) return { outcome: unchecked('receiver_invalid') };
        reader.setPackage(check.package);

        const moduleRead = readModule(check, rules.message);
        if (moduleRead.reason) return { outcome: unchecked(moduleRead.reason) };
        if (rules.message && LOCAL_RANGE.test(localRanges.get(packageNameOf(check.package)) ?? '')) {
          return { outcome: unchecked('module_workspace') };
        }
        // A type export has no value to read (carrick#2977): `readTypeReceiver` reads it.
        const typeReceiver = rules.message && check.receiver === TYPE_RECEIVER;
        const exportRead: Read<ExportRead> | undefined = typeReceiver ? undefined : readExport(check);
        if (exportRead !== undefined && 'reason' in exportRead) return { outcome: unchecked(exportRead.reason) };

        if (rules.http && exportRead !== undefined) {
          const key = exportKey(check);
          const receiverKey = `${key}\u0000${check.receiver}`;
          let receiverRead = httpReceivers.get(receiverKey);
          if (!receiverRead) {
            const factory = check.receiver.startsWith('instance:') ? check.receiver.slice('instance:'.length) : undefined;
            receiverRead = reader.readReceiver(
              exportRead.value.type,
              factory,
              factory === undefined ? undefined : factoryKeys.get(factoryKey(key, factory))
            );
            httpReceivers.set(receiverKey, receiverRead);
          }
          if ('reason' in receiverRead) return { outcome: unchecked(receiverRead.reason) };
          return { outcome: reader.judgeHttp(receiverRead.value, check.claim) };
        }

        if (check.claim.kind === 'op' && !rules.ops.has(check.claim.op)) {
          return { outcome: unchecked('role_unsupported') };
        }
        const receivers = exportRead === undefined ? readTypeReceiver(check) : readMessageReceivers(index, exportRead.value);
        if ('reason' in receivers) return { outcome: unchecked(receivers.reason) };
        const built = builtReceiver(check);
        const builtAtDefaults = built === undefined ? undefined : atDefaults(check, built);
        // An adapter's `runs` is read against the request's collections only
        // when today's reading refuses it (#2474 R6a): `judgeMake` asks.
        const collectionsOf = check.claim.kind === 'make' && check.claim.runs !== undefined ? collections : undefined;
        const holding: ts.Signature[] = [];
        const declarations: DeclaredAt[] = [];
        const members: ts.Declaration[] = [];
        const bodyMembers: ts.Declaration[] = [];
        // A request one receiver does not hold is dropped for the op (carrick#2968).
        let requestDropped: string | undefined;
        for (const receiver of receivers.value) {
          const result = reader.judgeMessage(receiver, check.claim, rules, builtAtDefaults, collectionsOf);
          if (result.outcome.verdict !== 'verified') return result;
          holding.push(...(result.holding ?? []));
          declarations.push(...(result.outcome.declarations ?? []));
          members.push(...(result.outcome.members ?? []));
          bodyMembers.push(...(result.outcome.bodyMembers ?? []));
          requestDropped ??= result.outcome.requestDropped;
        }
        const verified: Outcome =
          declarations.length > 0 ? { verdict: 'verified', declarations: sortedDeclarations(declarations) } : { ...VERIFIED };
        if (verified.verdict === 'verified' && members.length > 0) verified.members = members;
        if (verified.verdict === 'verified' && requestDropped !== undefined) verified.requestDropped = requestDropped;
        else if (verified.verdict === 'verified' && bodyMembers.length > 0) verified.bodyMembers = bodyMembers;
        return { outcome: verified, holding };
      };

      const semantics: SemanticsResult[] = [];
      for (let index = 0; index < checks.length; index++) {
        const check = checks[index];
        if (!judged.has(index) && performance.now() >= deadline) {
          semantics.push(stamp(check, unchecked('budget')));
          continue;
        }
        const outcome = judge(index).outcome;
        observe?.(check, outcome);
        semantics.push(stamp(check, outcome));
      }

      return {
        semantics,
        modules: [...moduleReads.values()].map(read => read.entry),
      };
    } finally {
      this.project.removeSourceFile(probe);
    }
  }

  /**
   * The members a service's server-framework labels name, read with the
   * verifier itself so only a label that verifies in the service's own
   * program counts (carrick#2674): each verified `send` op that carries a
   * `payload` slot, and each verified `receive` op whose `request` names a
   * `body` path. Other checks are not judged.
   */
  labelledMembers(
    fromDir: string,
    checks: LibraryCheck[],
    budgetMs: number,
    scanRoot?: string,
    typePackages: Readonly<Record<string, readonly string[]>> = {}
  ): LabelledMembers {
    const sends = new Map<string, ClaimSlot>();
    const bodies = new Set<string>();
    const wanted = checks.filter(
      check =>
        check.role === 'server_framework' &&
        check.claim.kind === 'op' &&
        ((check.claim.op === 'send' && check.claim.payload !== undefined) ||
          (check.claim.op === 'receive' && check.claim.request?.body !== undefined))
    );
    // A receiver an op is read on can need the maker or scope claims beside it.
    const context = checks.filter(
      check => check.role === 'server_framework' && (check.claim.kind === 'make' || check.claim.kind === 'scope')
    );
    if (wanted.length === 0) return { sends, bodies };
    const outcomes: Record<string, number> = {};
    const observe = (check: LibraryCheck, outcome: Outcome): void => {
      if (check.claim.kind !== 'op') return;
      const read =
        outcome.verdict !== 'verified'
          ? `${outcome.verdict} ${outcome.reason}`
          : outcome.requestDropped === undefined
            ? 'verified'
            : `verified request_dropped ${outcome.requestDropped}`;
      outcomes[read] = (outcomes[read] ?? 0) + 1;
      if (outcome.verdict !== 'verified') return;
      if (check.claim.op === 'send' && check.claim.payload !== undefined) {
        for (const member of outcome.members ?? []) sends.set(declarationKey(member), check.claim.payload);
      }
      for (const member of outcome.bodyMembers ?? []) bodies.add(declarationKey(member));
    };
    this.run(fromDir, [...context, ...wanted], budgetMs, scanRoot, { observe, readReplies: true, typePackages });
    return { sends, bodies, outcomes };
  }

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
  listSurface(
    fromDir: string,
    packages: string[],
    maxEntries: number,
    only: Readonly<Record<string, string[]>> = {},
    options: SurfaceOptions = {}
  ): ListedSurface {
    if (packages.length === 0) return { surfaces: [], surface_sha256: fullSurfaceSha256([]) };
    // Each specifier twice: as a namespace, which holds its named exports, and
    // as a default import, which is how a module that exports a value whole
    // (`export =`) is imported as `default`. A `lib:` key imports nothing: its
    // globals are read from the file it names.
    const imports: string[] = [];
    const importAt = packages.map((pkg, i) => {
      if (DEFAULT_LIBRARY.test(pkg)) return undefined;
      const at = imports.length;
      imports.push(`import * as __carrick_ns${i} from ${JSON.stringify(pkg)};`, importLine(pkg, 'default', `__carrick_d${i}`));
      return at;
    });
    const probeOf = (file: ts.SourceFile, i: number): SurfaceProbe => {
      const at = importAt[i];
      return at === undefined
        ? { defaultLibrary: packages[i] }
        : {
            namespace: file.statements[at] as ts.ImportDeclaration,
            defaultImport: file.statements[at + 1] as ts.ImportDeclaration,
          };
    };

    const read = this.readProbe(fromDir, imports, (_checker, reader, file) =>
      packages.map((pkg, i) => reader.makersOf(pkg, probeOf(file, i), only[pkg]))
    );

    const importLines: string[] = [];
    const buildLines: string[] = [];
    const builtIndex = new Map<string, number>();
    // The type-only exports a handler's parameter may be (carrick#2977): each
    // imported and declared at its defaults (`typeBuildLine`), as the verifier
    // reads a `type` receiver. Only with handlers: the table names them.
    const typeImports = new Map<string, number>();
    packages.forEach((pkg, i) => {
      if (options.handlers !== true && options.types !== true) return;
      for (const name of typeNamesOf(read[i], options)) {
        const local = `__carrick_e${importLines.length}`;
        const key = builtKey({ package: pkg, export: name }, TYPE_RECEIVER);
        typeImports.set(key, importLines.length);
        importLines.push(probeLine(pkg, name, local));
        builtIndex.set(key, buildLines.length);
        buildLines.push(typeBuildLine(buildLines.length, local));
      }
    });
    packages.forEach((pkg, i) => {
      for (const { export: name, receivers, scopes } of read[i].makers) {
        const local = `__carrick_e${importLines.length}`;
        importLines.push(probeLine(pkg, name, local));
        // The scope receivers a maker's members may return (carrick#2774) are
        // built beside the makers, so `listPackage` reads them as it reads those.
        for (const receiver of [...receivers, ...scopes]) {
          const expression = receiverExpression(local, receiver);
          if (expression === undefined) continue;
          builtIndex.set(builtKey({ package: pkg, export: name }, receiver), buildLines.length);
          buildLines.push(`${expression};`);
        }
      }
    });
    const firstBuild = imports.length + importLines.length;

    const surfaces = this.readProbe(fromDir, [...imports, ...importLines, ...buildLines], (checker, reader, file) => {
      // One budget for the whole call, spent across its specifiers: the answer is one line.
      reader.startListing(options.maxHandlerMembers ?? HANDLER_LISTING_BUDGET);
      return packages.map((pkg, i) => {
        const typeExports: BuiltType[] = [];
        if (options.handlers === true || options.types === true) {
          for (const name of typeNamesOf(read[i], options)) {
            const key = builtKey({ package: pkg, export: name }, TYPE_RECEIVER);
            const built = builtType(
              checker,
              file.statements[imports.length + typeImports.get(key)!],
              file.statements[firstBuild + builtIndex.get(key)!]
            );
            if (built) typeExports.push({ ...built, name });
          }
        }
        return reader.listPackage(pkg, probeOf(file, i), maxEntries, only[pkg], options, typeExports, (name, receiver) => {
          const index = builtIndex.get(builtKey({ package: pkg, export: name }, receiver));
          return index === undefined ? undefined : resolvedBuild(checker, file.statements[firstBuild + index]);
        });
      });
    });
    return { surfaces, surface_sha256: fullSurfaceSha256(surfaces) };
  }

  /** `read` over a probe file holding `lines` in `fromDir`, removed again afterwards. */
  private readProbe<T>(
    fromDir: string,
    lines: readonly string[],
    read: (checker: ts.TypeChecker, reader: DeclarationReader, file: ts.SourceFile) => T
  ): T {
    const probePath = path.join(fromDir, `__carrick_surface_probe_${process.pid}_${probeSequence++}.ts`);
    const probe = this.project.createSourceFile(probePath, `${lines.join('\n')}\n`, { overwrite: true });
    try {
      const program = this.project.getProgram().compilerObject;
      const file = program.getSourceFile(probe.getFilePath());
      if (!file) throw new Error(`probe file ${probePath} is not in the program`);
      const reader = new DeclarationReader(
        program,
        file,
        this.project.getModuleResolutionHost(),
        fromDir,
        undefined,
        this.graph
      );
      return read(program.getTypeChecker(), reader, file);
    } finally {
      this.project.removeSourceFile(probe);
    }
  }
}

/** A specifier's two imports in the surface probe (see `listSurface`), or the `lib:` key it names. */
type SurfaceProbe =
  | { namespace: ts.ImportDeclaration; defaultImport: ts.ImportDeclaration }
  | { defaultLibrary: string };

/** One value export a surface lists: its name, the declaration it names and its type. */
interface ListedExport {
  name: string;
  target: ts.Symbol;
  type: ts.Type;
}

/** An instance receiver the surface lists, with what the scope pass reads its members on (carrick#2774). */
interface ScopeSource {
  export: string;
  target: ts.Symbol;
  type: ts.Type;
  home: ReadonlySet<string>;
  receiver: SurfaceReceiver;
}

/** A way an export makes a receiver, by receiver id, with the library signatures that make it. */
interface Maker {
  receiver: string;
  /** Made by a member one level below the export, not by the export itself. */
  member: boolean;
  signatures: readonly ts.Signature[];
}

/**
 * What a probe statement that builds a receiver (`receiverExpression`)
 * resolves to: its maker or scope signature as a call with no argument and
 * no type argument instantiates it (see `DeclarationReader.returnOf`).
 */
function resolvedBuild(checker: ts.TypeChecker, statement: ts.Statement | undefined): ts.Signature | undefined {
  if (!statement || !ts.isExpressionStatement(statement)) return undefined;
  const expression = statement.expression;
  if (!ts.isCallExpression(expression) && !ts.isNewExpression(expression)) return undefined;
  return checker.getResolvedSignature(expression);
}

/** The receiver id a type export is read on (carrick#2977): the type itself, at its defaults. */
const TYPE_RECEIVER = 'type';

/** A type export read as receiver `type`: the symbol the import names and the type built at its defaults. */
interface BuiltType {
  name: string;
  symbol: ts.Symbol;
  type: ts.Type;
}

/** What `idOfSymbol` says a type is: its receiver id, and the export it sits on when the id says (module-declared types sit on the export that reaches them). */
interface TypeId {
  id: string;
  export?: string;
}

/** A type `types[]` may list (carrick#3050). */
interface TypeCandidate {
  id: string;
  export: string;
  symbol: ts.Symbol;
  type: ts.Type;
}

/** The probe line that builds a type export bound to `local` at its declared type-parameter defaults. */
function typeBuildLine(index: number, local: string): string {
  return `declare const __carrick_t${index}: ${local};`;
}

/**
 * What a type export's probe lines read (carrick#2977): the declaration its
 * import names (`importStatement`, one named import) and the type
 * `typeBuildLine` declares. Undefined when the name has no type meaning
 * (a `const`) or the line does not resolve to a type (a generic whose type
 * parameter has no default): the checker reads either as `any`.
 */
function builtType(
  checker: ts.TypeChecker,
  importStatement: ts.Statement | undefined,
  buildStatement: ts.Statement | undefined
): Omit<BuiltType, 'name'> | undefined {
  if (!importStatement || !ts.isImportDeclaration(importStatement)) return undefined;
  const bindings = importStatement.importClause?.namedBindings;
  const element = bindings && ts.isNamedImports(bindings) ? bindings.elements[0] : undefined;
  if (!element || !buildStatement || !ts.isVariableStatement(buildStatement)) return undefined;
  const local = checker.getSymbolAtLocation(element.name);
  if (!local) return undefined;
  const symbol = local.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(local) : local;
  if (checker.isUnknownSymbol(symbol) || (symbol.flags & ts.SymbolFlags.Type) === 0) return undefined;
  const declaration = buildStatement.declarationList.declarations[0];
  if (!declaration) return undefined;
  const type = checker.getTypeAtLocation(declaration.name);
  if (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) return undefined;
  return { symbol, type };
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
export function fullSurfaceSha256(surfaces: readonly LibrarySurface[]): string {
  const listed = surfaces
    .filter(surface => surface.reason === undefined && surface.exports.length > 0)
    .map(surface => [surface.package, surface.exports] as const)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  // A member's `handlers` (carrick#2674), a signature's `result_candidates`
  // (carrick#2817) and a member's `scope_receiver` (carrick#2774) are out of
  // the hash at every depth, and so is a member's `runtime_events`
  // (carrick#3032), so a package's key does not change when the lister
  // learns to list them.
  const text = JSON.stringify(listed, (key, value) =>
    key === 'handlers' || key === 'result_candidates' || key === 'scope_receiver' || key === 'runtime_events'
      ? undefined
      : value
  );
  return crypto.createHash('sha256').update(text).digest('hex');
}

function exportKey(check: { package: string; export: string }): string {
  return JSON.stringify([check.package, check.export]);
}

function factoryKey(exportKeyText: string, member: string): string {
  return `${exportKeyText}\u0000${member}`;
}

/**
 * `export`, `instance:()`, `instance:new`, `instance:new:<member>` or
 * `instance:<member>`, optionally followed by `>scope:<path.member>`; or
 * `type` (carrick#2977), followed by nothing.
 */
function parseReceiver(text: string): ReceiverPath | undefined {
  const steps = text.split('>');
  if (steps.length > 2) return undefined;
  const [base, scopeStep] = steps;
  let scope: string | undefined;
  if (scopeStep !== undefined) {
    const match = /^scope:(\S+)$/.exec(scopeStep);
    if (!match) return undefined;
    scope = match[1];
  }
  if (base === 'export') return { base, scope };
  if (base === TYPE_RECEIVER) return scope === undefined ? { base, type: true } : undefined;
  if (base === 'instance:()') return { base, scope, maker: { form: 'call', member: null } };
  if (base === 'instance:new') return { base, scope, maker: { form: 'new', member: null } };
  let match = /^instance:new:(\S+)$/.exec(base);
  if (match) return { base, scope, maker: { form: 'new', member: match[1] } };
  match = /^instance:(\S+)$/.exec(base);
  if (match && match[1] !== '()' && match[1] !== 'new') return { base, scope, maker: { form: 'call', member: match[1] } };
  return undefined;
}

/** The scope step a scope claim's results are read through: its path and member, joined by `.`. */
function scopeStep(claim: ScopeClaim): string {
  return [...(claim.path ?? []), claim.member].join('.');
}

/**
 * An op, scope or reserved name carries `on` or `of`, never both, and `of` is
 * a receiver a maker or a scope builds, by its receiver id, or `type`, the
 * export read as a type (carrick#2977): never the export itself, which
 * `on: 'export'` names (contract amendment 2, B3). "Both the export and one
 * maker's instances" is two elements, not one.
 */
function placementInvalid(claim: LibraryClaim): boolean {
  if (claim.kind === 'make' || claim.of === undefined) return false;
  if (claim.on !== undefined) return true;
  const of = parseReceiver(claim.of);
  return of === undefined || (of.maker === undefined && of.scope === undefined && of.type === undefined);
}

/**
 * Where a handler's request holds its path, method and body (#2474 R0,
 * carrick#2674) is read on a receive op with a handler, and names `path`,
 * `url` or `body`; every member path it gives is non-empty.
 */
function requestAtValid(claim: OpClaim): boolean {
  const request = claim.request!;
  if (claim.op !== 'receive' || claim.handler === undefined) return false;
  if (request.path === undefined && request.url === undefined && request.body === undefined) return false;
  return [request.path, request.url, request.method, request.body].every(
    members => members === undefined || members.length > 0
  );
}

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

/** A receiver's home packages, with the type-only packages the store lists for its package. */
function withTypePackages(home: ReadonlySet<string>, typePackages: readonly string[] | undefined): ReadonlySet<string> {
  return typePackages && typePackages.length > 0 ? new Set([...home, ...typePackages]) : home;
}

/**
 * A declaration's key within one process: its file as the compiler names it
 * and its start. Two programs of one project name a declaration alike, so a
 * label read in the verifier's probe program joins a member read in the
 * inferrer's (carrick#2674).
 */
export function declarationKey(declaration: ts.Node): string {
  const file = declaration.getSourceFile();
  return `${file.fileName}:${declaration.getStart(file)}`;
}

/**
 * The members a service's verified server-framework labels name, by
 * {@link declarationKey} (carrick#2674): each `send` op member with the slot
 * its payload sits in, and each member a receive op's `request.body` path
 * ends on.
 */
export interface LabelledMembers {
  sends: ReadonlyMap<string, ClaimSlot>;
  bodies: ReadonlySet<string>;
  /** How each label read came out (`verified`, or the reason it was not), for the log. */
  outcomes?: Readonly<Record<string, number>>;
}

/**
 * A message check names a receiver its claim can be read on (contract
 * amendment 2, B3). A maker is read on the export itself. An element with
 * `of` is read on exactly that receiver. One with `on` is read on the export
 * (`export`), on an instance of a maker (`instance`) or on either (`both`),
 * and never on a receiver a scope returns, which only `of` names. One with
 * neither names no receiver of its own. `type` is read only by `of: "type"`
 * (carrick#2977). A claim read on a receiver it was
 * not claimed for could verify a member of the same name with another shape
 * (`client.write(name, value)` against `instance.write(value)`).
 */
function fitsReceiver(check: LibraryCheck): boolean {
  const receiver = parseReceiver(check.receiver);
  if (!receiver) return false;
  const claim = check.claim;
  // A type export is read only by a claim placed on it (carrick#2977).
  if (receiver.type) return claim.kind !== 'make' && claim.of === TYPE_RECEIVER;
  if (claim.kind === 'make') return check.receiver === 'export';
  if (claim.of !== undefined) return check.receiver === claim.of;
  if (claim.on === undefined) return true;
  if (receiver.scope !== undefined) return false;
  if (claim.on === 'export') return receiver.maker === undefined;
  if (claim.on === 'instance') return receiver.maker !== undefined;
  return true;
}

/**
 * The receiver a maker or scope check builds: what the maker makes
 * (`instance:new`), or what the scope member returns on the receiver it is
 * read on (`instance:connect>scope:channel`). Undefined for an op or a
 * reserved name, and for a scope on a receiver a scope already returned.
 */
function builtReceiver(check: LibraryCheck): string | undefined {
  const claim = check.claim;
  if (claim.kind === 'make') {
    if (claim.member === null) return claim.form === 'call' ? 'instance:()' : 'instance:new';
    return claim.form === 'call' ? `instance:${claim.member}` : `instance:new:${claim.member}`;
  }
  if (claim.kind !== 'scope' || check.receiver.includes('>')) return undefined;
  return `${check.receiver}>scope:${scopeStep(claim)}`;
}

function builtKey(check: { package: string; export: string }, receiver: string): string {
  return JSON.stringify([check.package, check.export, receiver]);
}

/**
 * What a service writes to build `receiver` on the export bound to `local`,
 * passing no argument and no type argument: `new e()`, `e()`, `e.m()` or
 * `new e.m()`, then `.path.member()` for a scope step. Undefined for a
 * receiver id that does not parse.
 */
function receiverExpression(local: string, receiver: string): string | undefined {
  const parsed = parseReceiver(receiver);
  if (!parsed) return undefined;
  const access = (name: string) => (IDENTIFIER.test(name) ? `.${name}` : `[${JSON.stringify(name)}]`);
  let text = local;
  if (parsed.maker) {
    const callee = parsed.maker.member === null ? local : `${local}${access(parsed.maker.member)}`;
    text = parsed.maker.form === 'new' ? `new ${callee}()` : `${callee}()`;
  }
  if (parsed.scope !== undefined) text = `${text}${parsed.scope.split('.').map(access).join('')}()`;
  return text;
}

/**
 * The id of the receiver `member` of the receiver `receiver` returns when
 * called with a path (carrick#2774): `receiver>scope:member`. Undefined for a
 * receiver that is not an instance, or a member name the verifier's grammar
 * does not read back as one scope step.
 */
function scopeReceiverId(receiver: string, member: string): string | undefined {
  if (!receiver.startsWith('instance:') || receiver.includes('>') || /[>.\s]/.test(member)) return undefined;
  const id = `${receiver}>scope:${member}`;
  return parseReceiver(id)?.scope === member ? id : undefined;
}

/** The symbols that say who declared a type: its own, its alias's, its parts'. */
function typeSymbols(type: ts.Type): (ts.Symbol | undefined)[] {
  const symbols: (ts.Symbol | undefined)[] = [type.getSymbol(), type.aliasSymbol];
  if (type.isUnionOrIntersection()) {
    for (const part of type.types) symbols.push(part.getSymbol(), part.aliasSymbol);
  }
  return symbols;
}

/**
 * The probe's read of one export, bound to `local`: its import, or for a
 * `lib:` key the global itself, as `globalThis` holds it. A block-scoped
 * global (`declare const`) is not on `globalThis`; no default-library file
 * declares one.
 */
function probeLine(pkg: string, name: string, local: string): string {
  return DEFAULT_LIBRARY.test(pkg) ? globalRead(name, local) : importLine(pkg, name, local);
}

/** The probe's read of the global `name`, bound to `local`. */
function globalRead(name: string, local: string): string {
  const access = IDENTIFIER.test(name) ? `.${name}` : `[${JSON.stringify(name)}]`;
  return `const ${local} = globalThis${access};`;
}

/** The probe's import of one export, bound to `local`. */
function importLine(pkg: string, name: string, local: string): string {
  const specifier = JSON.stringify(pkg);
  if (name === 'default') return `import ${local} from ${specifier};`;
  const imported = IDENTIFIER.test(name) ? name : JSON.stringify(name);
  return `import { ${imported} as ${local} } from ${specifier};`;
}

/** Upper-case ASCII letters only: `ſ` and `ı` must not become `S` and `I`. */
function asciiUpperCase(text: string): string {
  return text.replace(/[a-z]/g, letter => letter.toUpperCase());
}

/**
 * The dependency ranges of the nearest package.json at or above `fromDir`, by
 * package name: a `workspace:`, `file:`, `link:` or `portal:` range names the
 * service's own source, which the verifier does not check yet (R2).
 */
function readLocalRanges(fromDir: string): ReadonlyMap<string, string> {
  const ranges = new Map<string, string>();
  let dir = path.resolve(fromDir);
  for (;;) {
    const manifest = path.join(dir, 'package.json');
    if (fs.existsSync(manifest)) {
      try {
        const parsed = JSON.parse(fs.readFileSync(manifest, 'utf8')) as Record<string, unknown>;
        for (const field of ['dependencies', 'devDependencies', 'peerDependencies', 'optionalDependencies']) {
          const deps = parsed[field];
          if (!deps || typeof deps !== 'object') continue;
          for (const [name, range] of Object.entries(deps as Record<string, unknown>)) {
            if (typeof range === 'string' && !ranges.has(name)) ranges.set(name, range);
          }
        }
      } catch {
        // An unreadable manifest names no ranges.
      }
      return ranges;
    }
    const parent = path.dirname(dir);
    if (parent === dir) return ranges;
    dir = parent;
  }
}

/** A part of a message claim. `base` and `prefix` are a maker's string parts. */
type PartName = 'name' | 'base' | 'prefix' | 'payload' | 'handler' | 'ack' | 'method' | 'runs' | 'mounts';

/** Where the parts of a message claim sit, and which argument holds which keys. */
interface ClaimLayout {
  /** Positional parts by argument. */
  positional: Map<number, PartName>;
  /** Keyed parts by argument, then by key. */
  keyed: Map<number, Map<string, PartName>>;
}

/**
 * Reads the declarations the probe imported, with the program's own checker.
 * Every predicate is the contract's (carrick#1564, section 3), stated on the
 * checker's public API, with the definitions the review amended.
 */
class DeclarationReader {
  private readonly checker: ts.TypeChecker;
  /** Names declaring files as position rows do: from the scan root, else the service root. */
  private readonly pathNames: PathNames;
  /** The service root, as a realpath. */
  private readonly root: string;
  private readonly packageDirectories = new Map<string, InstalledPackage | null>();
  private readonly realpaths = new Map<string, string>();
  /** Names the service declares in its own blocks, per augmented package (`global` for `declare global`). */
  private augmentedNames: ReadonlyMap<string, ReadonlySet<string>> | undefined;
  /** The package of the check being judged; see `serviceAugmentedNames`. */
  private currentPackage = '';
  /** `declaredProperties` per type, for `currentPackage` (its augmentations decide them). */
  private readonly declared = new Map<ts.Type, ts.Symbol[]>();
  /**
   * A handler parameter's listed members per type (`handlerMembersOf`), kept
   * across every specifier of one package and cleared with `declared` when
   * the package changes: a member's listing reads that package's
   * augmentations (carrick#2977).
   */
  private readonly handlerMembers = new Map<ts.Type, { members: SurfaceHandlerMember[]; dropped: number }>();
  /** `takesString` per signature, and `scopeMembers` per type and home: the same types recur across receivers. */
  private readonly takesStringRead = new Map<ts.Signature, boolean>();
  private readonly scopeMembersRead = new Map<ts.Type, Map<string, readonly string[]>>();
  /**
   * The surface being listed's `handler_types` (carrick#2977): its entries in
   * id order; per type object and export name, the id it was given (0: the
   * budget left it out); per content key, the id. Reset per surface.
   */
  private handlerTypes: SurfaceHandlerType[] = [];
  private readonly handlerTypeIds = new Map<ts.Type, Map<string | null, number>>();
  private readonly handlerTypeByContent = new Map<string, number>();
  /** The handler parameters the listing's budget left out of the surface being listed (carrick#3016). */
  private handlerParamsLeftOut = 0;
  /** The surface being listed's nameable type exports, by the symbol each names (carrick#2977). */
  private typeExportNames = new Map<ts.Symbol, string>();
  /** Handler-parameter members the rest of this `listSurface` call may still list (carrick#3016). */
  private handlerBudget = HANDLER_LISTING_BUDGET;
  /** The surface being listed lists `types[]` widened (`types: true`, carrick#3050). */
  private listTypes = false;
  /** The receivers being outlined are the surface's own: the types they return are candidates for `types[]`. */
  private collectTypes = false;
  /** The types `types[]` may list, by declaring symbol, in discovery order. */
  private readonly typeCandidates = new Map<ts.Symbol, TypeCandidate>();
  /** `idOfSymbol` per symbol, for the surface being listed. */
  private readonly typeIds = new Map<ts.Symbol, TypeId | null>();
  /** The surface's listed value exports, by target symbol: what a namespace-declared type's export is. */
  private exportTargets = new Map<ts.Symbol, string>();
  /** The packages the specifier names (itself and its types package): a type declared there is the specifier's own. */
  private specifierHome: ReadonlySet<string> = new Set();
  /** The export whose receivers are being outlined: the export a module-declared type is listed under. */
  private typeContext = '';
  /** `types[]` entries the budget left out of the surface being listed. */
  private typesLeftOut = 0;

  /** Begin one `listSurface` call: its specifiers share `handlerBudget` handler-parameter members. */
  startListing(handlerBudget: number): void {
    this.handlerBudget = handlerBudget;
  }
  /** Whether the package being listed lists each signature's `result_candidates`. */
  private listResultCandidates = false;
  /** The service root as given and as a realpath, longest first: what a listing prints as `<root>`. */
  private readonly printedRoots: readonly string[];
  /** `printed` per type: its text reads nothing but the type and `printedRoots` (carrick#2977). */
  private readonly printedTexts = new Map<ts.Type, string>();

  constructor(
    private readonly program: ts.Program,
    private readonly probe: ts.SourceFile,
    private readonly host: ts.ModuleResolutionHost,
    serviceRoot: string,
    scanRoot: string | undefined,
    private readonly graph: ModuleGraph
  ) {
    this.checker = program.getTypeChecker();
    this.pathNames = new PathNames(scanRoot ?? serviceRoot, graph.packageDirectoryOf);
    this.root = this.realpath(serviceRoot);
    this.printedRoots = [...new Set([this.root, serviceRoot])]
      .filter(root => root.length > 1)
      .sort((a, b) => b.length - a.length);
  }

  /**
   * A path with its symlinks resolved. The compiler spells the service's own
   * files as given and resolved dependencies as realpaths, so both sides of a
   * comparison go through here. A path not on disk (the default library ts-morph
   * serves from memory) is kept as it is.
   */
  private realpath(fileName: string): string {
    let real = this.realpaths.get(fileName);
    if (real === undefined) {
      try {
        real = fs.realpathSync(fileName);
      } catch {
        real = fileName;
      }
      this.realpaths.set(fileName, real);
    }
    return real;
  }

  // --------------------------------------------------------------------------
  // Resolve, export, receiver
  // --------------------------------------------------------------------------

  /**
   * Where the package resolved. The checker's module symbol is what the
   * service's program imports; its primary declaration must be the file the
   * module resolver lands on, and the resolver must have reached it as an
   * installed dependency. A `paths` alias to the service's own code, or a
   * `declare module` block the service writes, stands in for the package and
   * says nothing about it (`module_local`). The resolver's flag is read, not
   * `program.isSourceFileFromExternalLibrary`: once ts-morph has loaded a
   * dependency, the next program lists it as a root file and the program no
   * longer calls it external. `resolved_file` and `installed_version` both
   * come from that one agreeing answer.
   */
  readModule(pkg: string, declaration: ts.ImportDeclaration, runtimeModules = false): ModuleRead {
    const specifier = declaration.moduleSpecifier as ts.StringLiteral;
    const resolved =
      this.graph.resolveModule(pkg, this.probe.fileName, this.program.getCompilerOptions()) ??
      ts.resolveModuleName(
        pkg,
        this.probe.fileName,
        this.program.getCompilerOptions(),
        this.host,
        undefined,
        undefined,
        this.program.getModeForUsageLocation(this.probe, specifier)
      ).resolvedModule;
    const entry: SemanticsModule = { package: pkg };
    const done = (reason?: string): ModuleRead =>
      reason ? { entry: { ...entry, reason }, reason } : { entry };

    const moduleSymbol = this.checker.getSymbolAtLocation(specifier);
    const declarations = moduleSymbol?.declarations ?? [];
    const globalFile = declarations.length === 0 ? this.runtimeGlobalFile(pkg) : undefined;
    if (globalFile) {
      entry.resolved_file = globalFile.fileName;
      const version = this.installedPackage(globalFile.fileName)?.version;
      if (version) entry.installed_version = version;
      return { entry, globalFile };
    }
    if (runtimeModules && RUNTIME_MODULE.test(pkg)) return this.readRuntimeModule(declarations, entry, done);
    // The module itself, not a service-side augmentation of it.
    const primary = declarations.find(ts.isSourceFile) ?? declarations[0];
    const file = primary?.getSourceFile();
    if (file) {
      entry.resolved_file = file.fileName;
      const agrees = resolved !== undefined && this.isSameInstalledFile(resolved.resolvedFileName, file);
      // Under `node_modules` is not enough: a `paths` alias can point the name
      // at another installed package. The resolver must land in an installed
      // package directory, and that package must be the one named: by its
      // `packageId` (or its separate `@types` package), or, for an `npm:`
      // alias, by the directory name, which the installer takes from the
      // alias. (The package.json comparison beside it always holds:
      // TypeScript builds `packageId` from that same package.json.)
      const installed =
        agrees && resolved !== undefined ? this.installedPackage(resolved.resolvedFileName) : undefined;
      const packageId = resolved?.packageId;
      const named =
        installed !== undefined &&
        packageId !== undefined &&
        (isNamedPackage(pkg, packageId.name) ||
          (installed.directoryName === packageNameOf(pkg) && installed.packageName === packageId.name));
      if (named && packageId.version) entry.installed_version = packageId.version;
      if (!TYPESCRIPT_FILE.test(file.fileName)) return done('module_js_only');
      if (!named) return done('module_local');
      return done();
    }

    if (!resolved) return done('module_unresolved');
    entry.resolved_file = resolved.resolvedFileName;
    const version = resolved.packageId?.version;
    if (version) entry.installed_version = version;
    if (!TYPESCRIPT_FILE.test(resolved.resolvedFileName)) return done('module_js_only');
    // A declaration file with no module symbol exports nothing: every export
    // of it is missing.
    return done();
  }

  /**
   * `node:<module>`: the runtime's types package declares it in an ambient
   * `declare module` block, which no module resolution lands on (and which
   * the checker prefers over one that does). It is the runtime's when that
   * package, installed under the service, declares it; a block only the
   * service writes is `module_local`.
   */
  private readRuntimeModule(
    declarations: readonly ts.Declaration[],
    entry: SemanticsModule,
    done: (reason?: string) => ModuleRead
  ): ModuleRead {
    if (declarations.length === 0) return done('module_unresolved');
    const ambient = declarations.find(declaration => {
      const owner = this.packageOf(declaration.getSourceFile());
      return owner !== undefined && RUNTIME_TYPE_PACKAGES.has(owner);
    });
    if (!ambient) return done('module_local');
    const file = ambient.getSourceFile();
    entry.resolved_file = file.fileName;
    const version = this.installedPackage(file.fileName)?.version;
    if (version) entry.installed_version = version;
    return done();
  }

  /**
   * The resolver's file is the checker's file. With two installs of the same
   * name and version, TypeScript loads one and makes the other a redirect to
   * it, so the resolver's copy may be a redirect whose target is the checker's
   * file; that counts only when the target is itself an installed file.
   */
  private isSameInstalledFile(resolvedFileName: string, file: ts.SourceFile): boolean {
    const resolved = this.program.getSourceFile(resolvedFileName);
    if (resolved === file) return true;
    // `redirectInfo` is internal to the compiler, but it is the only record of
    // which copy TypeScript deduplicated a package into.
    const target = (resolved as { redirectInfo?: { redirectTarget: ts.SourceFile } } | undefined)
      ?.redirectInfo?.redirectTarget;
    return target === file && this.isInstalledFile(file.fileName);
  }

  /**
   * A `lib:` key: the default-library file it names, as the program's own
   * `lib` setting loads it, read by the compiler that built the program. A
   * file the program does not load is no module reason: each global answers
   * `export_missing` (`readGlobal`).
   */
  readDefaultLibrary(pkg: string): ModuleRead {
    const entry: SemanticsModule = { package: pkg, installed_version: ts.version };
    const file = this.defaultLibraryFile(pkg);
    if (file) entry.resolved_file = file.fileName;
    return { entry };
  }

  /**
   * The runtime types' file a `node:<path>` key names, when no module has
   * that name (carrick#2445): `node:web-globals/fetch` is the file
   * `web-globals/fetch.d.ts` (or `.../index.d.ts`) of the installed runtime
   * types package, which declares globals. Undefined when the program loads
   * no such file; the key is then a module name, read as one.
   */
  private runtimeGlobalFile(pkg: string): ts.SourceFile | undefined {
    if (!RUNTIME_MODULE.test(pkg)) return undefined;
    const relative = pkg.slice('node:'.length);
    return this.program.getSourceFiles().find(file => {
      const owner = this.packageOf(file);
      if (owner === undefined || !RUNTIME_TYPE_PACKAGES.has(owner)) return false;
      const name = this.realpath(file.fileName).split(path.sep).join('/');
      const marker = `/node_modules/${owner}/`;
      const inside = name.slice(name.lastIndexOf(marker) + marker.length);
      return inside === `${relative}.d.ts` || inside === `${relative}/index.d.ts`;
    });
  }

  /** The default-library file a `lib:` key names, when the program loads it. */
  defaultLibraryFile(pkg: string): ts.SourceFile | undefined {
    const name = pkg.slice('lib:'.length);
    return this.program
      .getSourceFiles()
      .find(file => this.program.isSourceFileDefaultLibrary(file) && path.basename(file.fileName) === name);
  }

  /**
   * `T` for a global (`probeLine`'s `globalThis` read): the type the probe's
   * local gets, and the global it names. The global must be a value the
   * key's lib file declares; one only another file declares (the runtime's
   * types, the service) is missing from it.
   */
  readGlobal(file: ts.SourceFile | undefined, statement: ts.Statement | undefined): Read<ExportRead> {
    const declaration =
      statement && ts.isVariableStatement(statement) ? statement.declarationList.declarations[0] : undefined;
    const access = declaration?.initializer;
    const name =
      access && ts.isPropertyAccessExpression(access)
        ? access.name
        : access && ts.isElementAccessExpression(access)
          ? access.argumentExpression
          : undefined;
    const target = name ? this.checker.getSymbolAtLocation(name) : undefined;
    if (
      !declaration ||
      !target ||
      !file ||
      !(target.flags & ts.SymbolFlags.Value) ||
      !(target.declarations ?? []).some(each => each.getSourceFile() === file)
    ) {
      return { reason: 'export_missing' };
    }
    const type = this.checker.getTypeAtLocation(declaration.name);
    if (this.isOpenTop(type)) return { reason: 'export_untyped' };
    return { value: { type, target } };
  }

  /** `T`: the type the probe's import gets, and the declaration it names. */
  readExport(declaration: ts.ImportDeclaration): Read<ExportRead> {
    const clause = declaration.importClause;
    const bindings = clause?.namedBindings;
    const local =
      clause?.name ??
      (bindings && ts.isNamedImports(bindings) ? bindings.elements[0]?.name : undefined);
    const alias = local ? this.checker.getSymbolAtLocation(local) : undefined;
    if (!local || !alias) return { reason: 'export_missing' };
    const target = this.checker.getAliasedSymbol(alias);
    // A name the module does not export resolves to the checker's unknown
    // symbol; a type-only export has no value to call.
    if (this.checker.isUnknownSymbol(target) || !(target.flags & ts.SymbolFlags.Value)) {
      return { reason: 'export_missing' };
    }
    const type = this.checker.getTypeOfSymbolAtLocation(alias, local);
    if (this.isOpenTop(type)) return { reason: 'export_untyped' };
    return { value: { type, target } };
  }

  /**
   * HTTP `R`: the export itself, or what its factory `factory` returns. The
   * instance comes from the first overload whose first parameter is an object
   * type (or the only overload). When a `factory` claim in the request for the
   * same factory holds, the overload that satisfies it must be that same one,
   * or the instance is unresolved: a base URL set through one overload says
   * nothing about the instance another overload builds. A factory claim that
   * holds on no overload leaves the rule as it is; the scanner never reads an
   * instance through it.
   */
  readReceiver(
    exported: ts.Type,
    factory: string | undefined,
    baseUrlKeys: ReadonlySet<string> | undefined
  ): Read<ts.Type> {
    if (factory === undefined) return { value: exported };
    const callable = this.callableProperty(exported, factory);
    if ('failure' in callable) return { reason: 'factory_unresolved' };
    const signatures = callable.signatures;
    const signature =
      signatures.find(sig => {
        const first = this.parameterAt(sig, 0);
        return first !== undefined && this.isObjectType(first);
      }) ?? (signatures.length === 1 ? signatures[0] : undefined);
    if (!signature) return { reason: 'factory_unresolved' };
    for (const baseUrlKey of baseUrlKeys ?? []) {
      const keyed = signatures.find(sig => this.factorySignatureOutcome(sig, baseUrlKey).rank === Infinity);
      if (keyed !== undefined && keyed !== signature) return { reason: 'factory_unresolved' };
    }
    const instance = this.checker.getReturnTypeOfSignature(signature);
    if (this.returnSaysNothing(instance)) return { reason: 'factory_unresolved' };
    return { value: instance };
  }

  /**
   * What a maker or scope signature returns. A generic one is read at its
   * declared type-parameter defaults (contract amendment 2, B6) when `built`,
   * the probe's build of that receiver with no argument and no type argument,
   * resolved to it: TypeScript instantiates such a call at each parameter's
   * default, else at its constraint when `unknown` does not satisfy it, else
   * at `unknown`, which says nothing. That is what a service that passes no
   * type argument gets; a type argument of its own only narrows which names
   * the slots allow. Otherwise the type parameters stay open.
   */
  returnOf(signature: ts.Signature, built?: ts.Signature): ts.Type {
    if (built !== undefined && this.instantiates(built, signature)) return this.checker.getReturnTypeOfSignature(built);
    return this.checker.getReturnTypeOfSignature(signature);
  }

  /**
   * `resolved` instantiates an overload that returns the very type
   * `signature` returns: `signature` itself, or another constructor of the
   * same generic class, which builds the same instance. Its type arguments
   * are then `signature`'s too. Another overload's instance says nothing
   * about this one's.
   */
  private instantiates(resolved: ts.Signature, signature: ts.Signature): boolean {
    // `target` is internal to the compiler, but it is the only record of
    // which overload a resolved call instantiated. Without it, nothing is
    // read at its defaults.
    const target = (resolved as { target?: ts.Signature }).target;
    return (
      target !== undefined &&
      this.checker.getReturnTypeOfSignature(target) === this.checker.getReturnTypeOfSignature(signature)
    );
  }

  /**
   * The packages whose declarations are a receiver's own: the named package
   * (and its `@types` package), and every package that declares one of
   * `symbols` (the export at the end of its re-export chain, the receiver's
   * type, its alias, its parts). A meta-package that re-exports a scoped core
   * package's client owns that client's members; a class that extends another
   * package's base does not own what it only inherits. The runtime's type
   * packages and the default library never join, unless the named package is
   * one of them. `base`, when given, is the home this one extends (a
   * sub-object's, along a claim's `path`).
   */
  homeOf(
    pkg: string,
    symbols: readonly (ts.Symbol | undefined)[],
    base?: ReadonlySet<string>
  ): ReadonlySet<string> {
    // A global's home is the default-library file set, and only that.
    if (DEFAULT_LIBRARY.test(pkg)) return base ?? new Set([DEFAULT_LIBRARY_HOME]);
    const named = packageNameOf(pkg);
    const home = new Set<string>(base ?? [named, typesPackageOf(named)]);
    const runtime =
      RUNTIME_MODULE.test(pkg) || RUNTIME_TYPE_PACKAGES.has(named) || RUNTIME_TYPE_PACKAGES.has(typesPackageOf(named));
    for (const symbol of symbols) {
      for (const declaration of symbol?.declarations ?? []) {
        const owner = this.packageOf(declaration.getSourceFile());
        if (owner === undefined) continue;
        if (!runtime && RUNTIME_TYPE_PACKAGES.has(owner)) continue;
        home.add(owner);
      }
    }
    return home;
  }

  /** The installed package a file belongs to, by its package.json name; never the default library. */
  private packageOf(file: ts.SourceFile): string | undefined {
    if (this.program.isSourceFileDefaultLibrary(file)) return undefined;
    const installed = this.installedPackage(file.fileName);
    return installed === undefined ? undefined : installed.packageName ?? installed.directoryName;
  }

  private isOwnFile(file: ts.SourceFile, home: ReadonlySet<string>): boolean {
    if (this.program.isSourceFileDefaultLibrary(file)) return home.has(DEFAULT_LIBRARY_HOME);
    const owner = this.packageOf(file);
    return owner !== undefined && home.has(owner);
  }

  // --------------------------------------------------------------------------
  // HTTP claims (#1564, unchanged)
  // --------------------------------------------------------------------------

  /**
   * An HTTP claim in the shared shape, judged by the #1564 check its shape
   * came from (`httpCheck` is the inverse). A shape no #1564 kind produces is
   * `claim_invalid`.
   */
  judgeHttp(receiver: ts.Type, claim: LibraryClaim): Outcome {
    if (claim.kind === 'result') return this.judgeHttpResult(receiver, claim);
    if (claim.kind === 'make') {
      const base = claim.base;
      if (
        claim.form !== 'call' ||
        claim.member === null ||
        claim.name !== undefined ||
        claim.handler !== undefined ||
        claim.prefix !== undefined ||
        base === undefined ||
        base.arg !== 0 ||
        base.key === undefined
      ) {
        return unchecked('claim_invalid');
      }
      return this.judgeFactory(receiver, claim.member, base.key);
    }
    if (claim.kind !== 'op' || claim.op !== 'request' || claim.handler || claim.ack || claim.path !== undefined) {
      return unchecked('claim_invalid');
    }
    const name = claim.name;
    if (name !== undefined && 'bound' in name) return unchecked('claim_invalid');
    if (claim.method_key) {
      const at = claim.method_key.arg;
      const methodKey = claim.method_key.key;
      if (methodKey === undefined || claim.method !== undefined || claim.options !== undefined) {
        return unchecked('claim_invalid');
      }
      const payload = claim.payload;
      if (at === 0) {
        if ((name && (name.arg !== 0 || name.key === undefined)) || (payload && (payload.arg !== 0 || payload.key === undefined))) {
          return unchecked('claim_invalid');
        }
        const selected = this.selectRequest(receiver, claim.member, 'config', name?.key, methodKey, payload?.key);
        return 'failure' in selected ? selected.failure : VERIFIED;
      }
      if (at === 1) {
        if (!name || name.arg !== 0 || name.key !== undefined || (payload && (payload.arg !== 1 || payload.key === undefined))) {
          return unchecked('claim_invalid');
        }
        const selected = this.selectRequest(receiver, claim.member, 'path_options', undefined, methodKey, payload?.key);
        return 'failure' in selected ? selected.failure : VERIFIED;
      }
      return unchecked('claim_invalid');
    }
    if (claim.member === null || !name || name.arg !== 0 || name.key !== undefined) return unchecked('claim_invalid');
    const hasBody = claim.payload !== undefined || claim.options !== undefined;
    if (claim.method === undefined && !hasBody) return unchecked('claim_invalid');
    if (claim.method !== undefined) {
      const verb = this.judgeVerb(receiver, claim.member, claim.method);
      if (verb.verdict !== 'verified' || !hasBody) return verb;
    }
    if (claim.options !== undefined) {
      const payload = claim.payload;
      if (claim.options.arg !== 1 || claim.options.key !== undefined || (payload && (payload.arg !== 1 || payload.key === undefined))) {
        return unchecked('claim_invalid');
      }
      return this.judgeVerbBody(receiver, claim.member, 'path_options', payload?.key);
    }
    if (claim.payload!.arg !== 1 || claim.payload!.key !== undefined) return unchecked('claim_invalid');
    return this.judgeVerbBody(receiver, claim.member, 'path_body', undefined);
  }

  /**
   * `member` is a declared callable property of the receiver; some signature's
   * first parameter declares `base_url_key` accepting string, and returns a
   * type that says something.
   */
  private judgeFactory(receiver: ts.Type, member: string, baseUrlKey: string): Outcome {
    const callable = this.callableProperty(receiver, member);
    if ('failure' in callable) return callable.failure;
    let best: RankedFailure | undefined;
    for (const signature of callable.signatures) {
      const result = this.factorySignatureOutcome(signature, baseUrlKey);
      if (result.rank === Infinity) return VERIFIED;
      best = furthest(best, result.rank, result.outcome);
    }
    return best!.outcome;
  }

  /** One factory overload against the factory predicate; rank `Infinity` holds. */
  private factorySignatureOutcome(signature: ts.Signature, baseUrlKey: string): RankedFailure {
    const first = this.keyParameterAt(signature, 0);
    if (first === undefined) return { rank: 1, outcome: failed('param_missing') };
    const key = this.keyProperty(first, baseUrlKey, type => this.acceptsString(type));
    if (!key) return { rank: 2, outcome: this.slotFailure(first, 'key_missing') };
    const keyType = this.checker.getTypeOfSymbol(key);
    if (!this.acceptsString(keyType)) return { rank: 3, outcome: this.slotFailure(keyType, 'key_not_string') };
    // The option is there, but what the factory builds cannot be read, so
    // nothing it returns can be checked either.
    if (this.returnSaysNothing(this.checker.getReturnTypeOfSignature(signature))) {
      return { rank: 4, outcome: unchecked('factory_unresolved') };
    }
    return { rank: Infinity, outcome: VERIFIED };
  }

  /**
   * `method` is `member` upper-cased (ASCII only) and an HTTP method (a method
   * is not a type-level fact, so this is read off the claim itself); `member`
   * is a declared callable property whose first parameter accepts string.
   */
  private judgeVerb(receiver: ts.Type, member: string, method: string): Outcome {
    if (method !== asciiUpperCase(member) || !HTTP_METHODS.has(method)) {
      return failed('method_not_member_verb');
    }
    const callable = this.callableProperty(receiver, member);
    if ('failure' in callable) return callable.failure;
    let best: RankedFailure | undefined;
    for (const signature of callable.signatures) {
      const first = this.keyParameterAt(signature, 0);
      if (first !== undefined && this.acceptsString(first)) return VERIFIED;
      best = furthest(
        best,
        1,
        first === undefined ? failed('path_not_string') : this.slotFailure(first, 'path_not_string')
      );
    }
    return best!.outcome;
  }

  /**
   * Some signature whose first parameter accepts string has a second one:
   * open for `path_body`; for `path_options`, an object type with at least one
   * declared property, `body_key` among them when given.
   */
  private judgeVerbBody(
    receiver: ts.Type,
    member: string,
    args: 'path_body' | 'path_options',
    bodyKey: string | undefined
  ): Outcome {
    const callable = this.callableProperty(receiver, member);
    if ('failure' in callable) return callable.failure;
    let best: RankedFailure | undefined;
    for (const signature of callable.signatures) {
      const first = this.keyParameterAt(signature, 0);
      // An open body is read as declared: a type parameter is the open body
      // itself, not its constraint.
      const second =
        args === 'path_body' ? this.parameterAt(signature, 1) : this.keyParameterAt(signature, 1);
      if (first === undefined || second === undefined) {
        best = furthest(best, 1, failed('param_missing'));
        continue;
      }
      if (!this.acceptsString(first)) {
        best = furthest(best, 1, this.slotFailure(first, 'param_missing'));
        continue;
      }
      if (args === 'path_body') {
        if (this.isOpenBody(second)) return VERIFIED;
        best = furthest(best, 2, this.slotFailure(second, 'body_not_open'));
        continue;
      }
      if (!this.isObjectType(second) || this.declaredProperties(second).length === 0) {
        best = furthest(best, 2, this.slotFailure(second, 'options_not_object'));
        continue;
      }
      if (bodyKey !== undefined && !this.declaredProperty(second, bodyKey)) {
        best = furthest(best, 3, failed('key_missing'));
        continue;
      }
      return VERIFIED;
    }
    return best!.outcome;
  }

  /**
   * The callee's signatures a request claim can be read through. The callee
   * is `receiver[member]`, or the receiver itself when `member` is null.
   *
   * `config`: the first parameter is an object type declaring `url_key`
   * accepting string and `method_key` accepting string or an HTTP method
   * literal. `path_options`: the first parameter accepts string and the second
   * is an object type declaring `method_key` accepting the same. A
   * `request_body` claim also needs `body_key` declared there.
   *
   * All the keys come from ONE config object: for a union, from one member
   * that is an object type and not a function type. Keys split across union
   * members (`{ url } | { method }`) describe no call anyone can make.
   */
  private selectRequest(
    receiver: ts.Type,
    member: string | null,
    args: RequestArgs,
    urlKey: string | undefined,
    methodKey: string,
    bodyKey?: string
  ): { signatures: readonly ts.Signature[] } | { failure: Outcome } {
    const callable =
      member === null ? this.callSignatures(receiver) : this.callableProperty(receiver, member);
    if ('failure' in callable) return callable;

    const selected: ts.Signature[] = [];
    let best: RankedFailure | undefined;
    for (const signature of callable.signatures) {
      const first = this.keyParameterAt(signature, 0);
      let config: Slot | undefined = first;
      if (args === 'path_options') {
        if (first === undefined || !this.acceptsString(first)) {
          best = furthest(best, 1, first === undefined ? failed('param_missing') : this.slotFailure(first, 'param_missing'));
          continue;
        }
        config = this.keyParameterAt(signature, 1);
      }
      if (config === undefined) {
        best = furthest(best, 1, failed('param_missing'));
        continue;
      }
      if (config === VARIADIC || !this.isObjectType(config)) {
        best = furthest(best, 1, this.slotFailure(config, 'param_missing'));
        continue;
      }
      const parts = this.parts(config);
      const views = parts.length > 1 ? parts.filter(part => this.isPlainObject(part)) : [config];
      if (views.length === 0) best = furthest(best, 2, failed('key_missing'));
      for (const view of views) {
        const outcome = this.requestKeysOutcome(view, args, urlKey, methodKey, bodyKey);
        if (outcome.rank === Infinity) {
          selected.push(signature);
          break;
        }
        best = furthest(best, outcome.rank, outcome.outcome);
      }
    }
    if (selected.length > 0) return { signatures: selected };
    return { failure: best!.outcome };
  }

  /** One config object against a request claim's keys; rank `Infinity` holds. */
  private requestKeysOutcome(
    config: Slot,
    args: RequestArgs,
    urlKey: string | undefined,
    methodKey: string,
    bodyKey: string | undefined
  ): RankedFailure {
    // A `config` claim names its url key; with none there is nothing to find.
    const url =
      args === 'config'
        ? urlKey === undefined
          ? undefined
          : this.declaredProperty(config, urlKey)
        : null;
    const method = this.declaredProperty(config, methodKey);
    if (url === undefined || !method) return { rank: 2, outcome: failed('key_missing') };
    const urlType = url === null ? undefined : this.checker.getTypeOfSymbol(url);
    if (urlType !== undefined && !this.acceptsString(urlType)) {
      return { rank: 3, outcome: this.slotFailure(urlType, 'key_not_string') };
    }
    const methodType = this.checker.getTypeOfSymbol(method);
    if (!this.acceptsMethod(methodType)) {
      return { rank: 3, outcome: this.slotFailure(methodType, 'key_not_string') };
    }
    if (bodyKey !== undefined && !this.declaredProperty(config, bodyKey)) {
      return { rank: 4, outcome: failed('key_missing') };
    }
    return { rank: Infinity, outcome: VERIFIED };
  }

  // --------------------------------------------------------------------------
  // Message claims (broker, in-process bus, socket)
  // --------------------------------------------------------------------------

  /**
   * A message claim on its receiver; a maker or scope claim also returns the
   * overloads it holds on. `built` is the probe's no-argument build of the
   * receiver a maker or scope claim makes (see `returnOf`).
   */
  judgeMessage(
    receiver: MessageReceiver,
    claim: LibraryClaim,
    rules: RoleRules,
    built?: ts.Signature,
    collections?: () => readonly ts.Type[] | undefined
  ): Judged {
    switch (claim.kind) {
      case 'make':
        return this.judgeMake(receiver, claim, built, collections);
      case 'scope':
        return this.judgeScope(receiver, claim, built);
      case 'op':
        return { outcome: this.judgeOp(receiver, claim, rules) };
      case 'reserved':
        return { outcome: this.judgeReserved(receiver, claim) };
      case 'result':
        return { outcome: this.resultOutcome(this.ownCallee(receiver, claim.path ?? [], claim.member, 'call'), claim.result) };
    }
  }

  /**
   * A `result` claim (carrick#2817): the claimed result is a candidate of at
   * least one signature the callee declares, by rule R (`result-candidates.ts`).
   * Anything else, including a call that returns nothing, is
   * `result_not_candidate`.
   */
  private resultOutcome(callee: Callable, shape: ResultShape): Outcome {
    if ('failure' in callee) return callee.failure;
    for (const signature of callee.signatures) {
      const candidates = resultCandidatesOf(this.checker, signature);
      if (candidates !== undefined && holdsResult(candidates, shape)) return VERIFIED;
    }
    return failed('result_not_candidate');
  }

  /** A `result` claim on an HTTP receiver: `path` walked from it, then `member` of what that reaches. */
  private judgeHttpResult(receiver: ts.Type, claim: Extract<LibraryClaim, { kind: 'result' }>): Outcome {
    let holder = receiver;
    for (const hop of claim.path ?? []) {
      const property = this.declaredProperty(holder, hop);
      if (!property) return failed('member_missing');
      holder = this.checker.getTypeOfSymbol(property);
    }
    const callee = claim.member === null ? this.callSignatures(holder) : this.callableProperty(holder, claim.member);
    return this.resultOutcome(callee, claim.result);
  }

  /**
   * `member` (or the export itself, when null) is callable (`call`) or
   * constructible (`new`) through signatures the receiver's home packages
   * declare; some overload takes the base, prefix, name and handler where the
   * claim's slots put them (contract amendment 2, B2), under the name-slot
   * rules, and returns a type that says something, read at its type-parameter
   * defaults (`returnOf`). Those overloads are the ones an instance is read
   * through. A definition (`task({ id, run })`) is a maker with a `name` and a
   * `handler` slot; a queue (`new Queue("emails")`) one with a positional name.
   *
   * An adapter (#2474 R6a, contract amendment 3 on carrick#2434) is a maker
   * whose `runs` slot takes a collection rather than a function. Today's
   * reading runs first, unchanged; only an overload it refuses on `runs` is
   * read again, when the request holds verified `collects` scopes
   * (`collections`), with `runs` taking what one of them returns
   * (`runsCollectionOutcome`). With no such scope this arm never runs.
   */
  private judgeMake(
    receiver: MessageReceiver,
    claim: MakeClaim,
    built?: ts.Signature,
    collections?: () => readonly ts.Type[] | undefined
  ): Judged {
    const layout = this.layout({
      base: claim.base,
      prefix: claim.prefix,
      name: claim.name,
      handler: claim.handler,
      runs: claim.runs,
    });
    if ('failure' in layout) return { outcome: layout.failure };
    const callee = this.ownCallee(receiver, [], claim.member, claim.form);
    if ('failure' in callee) return { outcome: callee.failure };
    const labels = claim.key_labels ?? {};
    const holding: ts.Signature[] = [];
    let best: RankedFailure | undefined;
    for (const signature of callee.signatures) {
      let result = this.messageSignatureOutcome(signature, layout.value, labels);
      if (result.outcome.verdict !== 'verified' && result.outcome.reason.startsWith('runs_') && collections !== undefined) {
        const collected = collections();
        if (collected !== undefined) {
          result = this.messageSignatureOutcome(signature, layout.value, labels, declared =>
            this.runsCollectionOutcome(signature, declared, collected)
          );
        }
      }
      if (result.rank === Infinity && this.returnSaysNothing(this.returnOf(signature, built))) {
        result = { rank: 6, outcome: unchecked('maker_unresolved') };
      }
      if (result.rank === Infinity) holding.push(signature);
      else best = furthest(best, result.rank, result.outcome);
    }
    if (holding.length > 0) return { outcome: VERIFIED, holding };
    return { outcome: best!.outcome };
  }

  /**
   * `member` (after `path`) is a home-declared callable member whose name
   * slot accepts a string, under the name-slot rules, and returns a type that
   * says something, read at its type-parameter defaults (`returnOf`).
   *
   * Route fields (#2474 R0, contract amendment of 2026-10-22 on carrick#2434):
   * - `runs` with `yields`: the member runs the function at `runs` (a
   *   function with a declared signature) and hands it, at `yields.param`,
   *   an instance of the package's export `yields.export` (`yieldsOutcome`);
   * - `mounts`: the receiver the claim is read on fits the parameter there
   *   (`mountsOutcome`).
   * Either one binds its instance through a parameter or an argument, not
   * the member's return, so the return is not read and the claim holds no
   * overloads: a `>scope:` receiver read through it is not resolved.
   * `runs` without `yields` (or the reverse), or `mounts` beside either, is
   * `claim_invalid`.
   *
   * Procedure routers (#2474 R6a, contract amendment 3 on carrick#2434): a
   * scope carries exactly one of `name` and `collects`, and `collects` never
   * beside `runs`, `yields` or `mounts` (`claim_invalid`). With `collects`
   * the member takes an object of procedures there (`collectsOutcome`), and
   * its return, the collection, is read as a plain scope's is.
   */
  private judgeScope(receiver: MessageReceiver, claim: ScopeClaim, built?: ts.Signature): Judged {
    if ((claim.runs === undefined) !== (claim.yields === undefined)) return { outcome: unchecked('claim_invalid') };
    if (claim.mounts !== undefined && claim.runs !== undefined) return { outcome: unchecked('claim_invalid') };
    if ((claim.name === undefined) === (claim.collects === undefined)) return { outcome: unchecked('claim_invalid') };
    if (claim.collects !== undefined && (claim.runs !== undefined || claim.mounts !== undefined)) {
      return { outcome: unchecked('claim_invalid') };
    }
    const binds = claim.runs !== undefined || claim.mounts !== undefined;
    const layout = this.layout({ name: claim.name, runs: claim.runs, mounts: claim.mounts });
    if ('failure' in layout) return { outcome: layout.failure };
    const callee = this.ownCallee(receiver, claim.path ?? [], claim.member, 'call');
    if ('failure' in callee) return { outcome: callee.failure };
    const labels = claim.key_labels ?? {};
    const holding: ts.Signature[] = [];
    let best: RankedFailure | undefined;
    for (const signature of callee.signatures) {
      let result = this.messageSignatureOutcome(signature, layout.value, labels);
      if (result.rank === Infinity && claim.yields !== undefined) {
        const given = this.yieldsOutcome(signature, claim.runs!, claim.yields, receiver);
        if (given) result = { rank: 5, outcome: given };
      }
      if (result.rank === Infinity && claim.mounts !== undefined) {
        const mounted = this.mountsOutcome(signature, claim.mounts, receiver);
        if (mounted) result = { rank: 5, outcome: mounted };
      }
      if (result.rank === Infinity && claim.collects !== undefined) {
        const collected = this.collectsOutcome(signature, claim.collects);
        if (collected) result = { rank: 5, outcome: collected };
      }
      if (result.rank === Infinity && !binds && this.returnSaysNothing(this.returnOf(signature, built))) {
        result = { rank: 6, outcome: unchecked('scope_unresolved') };
      }
      if (result.rank === Infinity) holding.push(signature);
      else best = furthest(best, result.rank, result.outcome);
    }
    if (holding.length > 0) {
      // The member's own declarations, as a verified op carries them (#2636).
      const declarations = this.declarationsOf({ signatures: holding, named: callee.named });
      const outcome: Outcome = declarations.length > 0 ? { verdict: 'verified', declarations } : VERIFIED;
      return binds ? { outcome } : { outcome, holding };
    }
    return { outcome: best!.outcome };
  }

  /**
   * `mounts: { arg, key? }` on a scope (#2474 R0): the receiver the claim is
   * read on is assignable to the parameter there, so an instance like it can
   * be served under the scope's name. A parameter typed `any`, `unknown`,
   * `{}` or by an unconstrained type parameter says nothing
   * (`mounts_untyped`). There is no separate "not a function" rule: a router
   * type that is itself a handler is a common shape, and a parameter only a
   * function fits fails here because the instance does not fit it.
   * Undefined when it holds.
   */
  private mountsOutcome(signature: ts.Signature, mounts: ClaimSlot, receiver: Holder): Outcome | undefined {
    let declared = this.keyParameterAt(signature, mounts.arg);
    if (declared !== undefined && mounts.key !== undefined) {
      const property = this.declaredProperty(declared, mounts.key);
      declared = property ? this.checker.getTypeOfSymbol(property) : undefined;
    }
    if (declared === undefined) return failed('param_missing');
    const slot = this.throughConstraint(declared);
    if (slot === VARIADIC) return unchecked('mounts_untyped');
    const parts = this.parts(slot);
    if (parts.length === 0) return failed('mounts_not_instance');
    if (parts.some(part => this.isOpenTop(part) || this.isUnconstrained(part) || this.isEmptyObject(part))) {
      return unchecked('mounts_untyped');
    }
    const erased = this.fitsErased(receiver.type, slot, signature.typeParameters ?? []);
    if (erased !== undefined) return erased ? undefined : failed('mounts_not_instance');
    return this.checker.isTypeAssignableTo(receiver.type, slot) ? undefined : failed('mounts_not_instance');
  }

  /**
   * `collects: { arg, key?, join }` on a scope (#2474 R6a): the parameter
   * there is an object type that is not a function, whose keys name the
   * procedures. A parameter typed `any`, `unknown`, `{}` or by an
   * unconstrained type parameter says nothing (`collects_untyped`).
   * Undefined when it holds.
   */
  private collectsOutcome(signature: ts.Signature, collects: ClaimSlot): Outcome | undefined {
    let declared = this.keyParameterAt(signature, collects.arg);
    if (declared !== undefined && collects.key !== undefined) {
      const property = this.declaredProperty(declared, collects.key);
      declared = property ? this.throughConstraint(this.checker.getTypeOfSymbol(property)) : undefined;
    }
    if (declared === undefined) return failed('param_missing');
    if (declared === VARIADIC) return unchecked('collects_untyped');
    const parts = this.parts(declared);
    if (parts.length === 0) return failed('collects_not_object');
    if (parts.some(part => this.isOpenTop(part) || this.isUnconstrained(part) || this.isEmptyObject(part))) {
      return unchecked('collects_untyped');
    }
    if (parts.some(part => !this.isObjectLike(part) || part.getCallSignatures().length > 0)) {
      return failed('collects_not_object');
    }
    return undefined;
  }

  /**
   * An adapter's `runs` slot (#2474 R6a) takes one of `collections`, the
   * types the request's verified `collects` scopes return, read as a mounted
   * instance is (`fitsErased`, then assignability). A slot typed `any`,
   * `unknown`, `{}` or by an unconstrained type parameter says nothing
   * (`runs_untyped`). Undefined when it holds.
   */
  private runsCollectionOutcome(
    signature: ts.Signature,
    declared: Slot | undefined,
    collections: readonly ts.Type[]
  ): Outcome | undefined {
    if (declared === undefined) return failed('param_missing');
    const slot = this.throughConstraint(declared);
    if (slot === VARIADIC) return unchecked('runs_untyped');
    const parts = this.parts(slot);
    if (parts.length === 0) return failed('runs_not_collection');
    if (parts.some(part => this.isOpenTop(part) || this.isUnconstrained(part) || this.isEmptyObject(part))) {
      return unchecked('runs_untyped');
    }
    const own = signature.typeParameters ?? [];
    for (const collection of collections) {
      const erased = this.fitsErased(collection, slot, own);
      if (erased === true) return undefined;
      if (erased === undefined && this.checker.isTypeAssignableTo(collection, slot)) return undefined;
    }
    return failed('runs_not_collection');
  }

  /**
   * `receiver` fits `slot` with the signature's own type parameters
   * (`own`) erased to their constraints (carrick#2652), or undefined when
   * that reading does not apply. The receiver is built at its defaults, so a
   * parameter typed `Tree<SubEnv, SubBase>` has free arguments that no default
   * instance is assignable to. It applies when `slot` and `receiver` are
   * references to the same generic type and no argument mixes an own type
   * parameter into a larger type: an argument that is an own type parameter
   * takes any receiver argument its constraint takes (none: any), every other
   * argument must be the receiver's.
   */
  private fitsErased(receiver: ts.Type, slot: ts.Type, own: readonly ts.TypeParameter[]): boolean | undefined {
    if (own.length === 0) return undefined;
    const from = this.referenceOf(receiver);
    const to = this.referenceOf(slot);
    if (from === undefined || to === undefined) return undefined;
    const count = (to.target.typeParameters ?? []).length;
    const given = this.argumentsAs(from.target, this.checker.getTypeArguments(from), to.target);
    const wanted = this.checker.getTypeArguments(to);
    if (count === 0 || given === undefined || given.length < count || wanted.length < count) return undefined;
    for (let i = 0; i < count; i++) {
      if (own.includes(wanted[i] as ts.TypeParameter)) {
        const constraint = this.checker.getBaseConstraintOfType(wanted[i]);
        if (constraint === undefined || this.isOpenTop(constraint)) continue;
        if (own.some(parameter => this.mentions(constraint, parameter))) return undefined;
        if (!this.checker.isTypeAssignableTo(given[i], constraint)) return false;
      } else if (own.some(parameter => this.mentions(wanted[i], parameter))) {
        return undefined;
      } else if (
        given[i] !== wanted[i] &&
        !(this.checker.isTypeAssignableTo(given[i], wanted[i]) && this.checker.isTypeAssignableTo(wanted[i], given[i]))
      ) {
        return false;
      }
    }
    return true;
  }

  /**
   * The type arguments the instance `current<args>` has as an instance of
   * `target`: its own when it is one, else through the base types its class
   * extends, where each base argument is a type parameter of the class
   * (replaced by the matching argument) or names none. Undefined when
   * `target` is not reached that way.
   */
  private argumentsAs(
    current: ts.GenericType,
    args: readonly ts.Type[],
    target: ts.GenericType,
    depth = 0
  ): readonly ts.Type[] | undefined {
    if (current === target) return args;
    if (depth > 6) return undefined;
    const parameters = current.typeParameters ?? [];
    for (const base of this.checker.getBaseTypes(current)) {
      const reference = this.referenceOf(base);
      if (reference === undefined) continue;
      const mapped: ts.Type[] = [];
      let usable = true;
      for (const argument of this.checker.getTypeArguments(reference)) {
        const index = parameters.indexOf(argument as ts.TypeParameter);
        if (index >= 0) mapped.push(args[index]);
        else if (parameters.some(parameter => this.mentions(argument, parameter))) usable = false;
        else mapped.push(argument);
      }
      const found = usable ? this.argumentsAs(reference.target, mapped, target, depth + 1) : undefined;
      if (found !== undefined) return found;
    }
    return undefined;
  }

  private referenceOf(type: ts.Type): ts.TypeReference | undefined {
    if (!(type.flags & ts.TypeFlags.Object)) return undefined;
    return (type as ts.ObjectType).objectFlags & ts.ObjectFlags.Reference ? (type as ts.TypeReference) : undefined;
  }

  /** `type` names `parameter` anywhere in its arguments or members of a union or intersection. */
  private mentions(type: ts.Type, parameter: ts.TypeParameter, depth = 0): boolean {
    if (type === parameter) return true;
    if (depth > 6) return true;
    if (type.isUnionOrIntersection()) return type.types.some(part => this.mentions(part, parameter, depth + 1));
    // A deferred conditional (`Server extends X ? A : B`) or an alias
    // instance names its parameter in its check type or its alias arguments
    // (carrick#2663).
    if (type.aliasTypeArguments?.some(argument => this.mentions(argument, parameter, depth + 1))) return true;
    if (type.flags & ts.TypeFlags.Conditional) {
      const conditional = type as ts.ConditionalType;
      return [conditional.checkType, conditional.extendsType].some(part => this.mentions(part, parameter, depth + 1));
    }
    const reference = this.referenceOf(type);
    if (reference !== undefined) {
      return this.checker.getTypeArguments(reference).some(argument => this.mentions(argument, parameter, depth + 1));
    }
    return false;
  }

  /**
   * A send, receive or request op: the home-declared callable member (after
   * `path`; or the receiver itself, when null) with some overload that takes
   * the name, payload, handler and acknowledgement where the claim puts them.
   * A name bound by the maker or the scope must have been bound: the
   * instance's maker claim has a `name` slot, or the receiver came through a
   * scope.
   */
  private judgeOp(receiver: MessageReceiver, claim: OpClaim, rules: RoleRules): Outcome {
    const method = this.methodOutcome(claim, rules);
    if (method) return method;
    // What a handler is handed is read on a receive op's handler only.
    if (claim.yields !== undefined && (claim.op !== 'receive' || claim.handler === undefined)) {
      return unchecked('claim_invalid');
    }
    // So is where the handler's request holds its path and method (#2474 R0).
    if (claim.request !== undefined && !requestAtValid(claim)) return unchecked('claim_invalid');
    const name = claim.name;
    // A send carries a payload: a name alone on `send(data)` is the data. An
    // op that runs a function or client it is handed takes its name from
    // what it runs, and one that hands its handler a receiver (a server's
    // connection event) names nothing on the wire itself, so neither needs a
    // name of its own.
    const nameless =
      claim.runs !== undefined || claim.yields !== undefined || (claim.op === 'send' && rules.namelessSends === true);
    if ((name === undefined && !nameless) || (claim.op === 'send' && claim.payload === undefined)) {
      return unchecked('claim_invalid');
    }
    let nameSlot: ClaimSlot | undefined;
    if (name === undefined) {
      nameSlot = undefined;
    } else if ('bound' in name) {
      const bound =
        name.bound === 'maker' ? receiver.makerBindsName : name.bound === 'scope' ? receiver.scoped : receiver.keyBound === true;
      if (!bound) return failed('name_unbound');
    } else {
      nameSlot = name;
    }
    const runs = claim.runs;
    const boundRuns = runs?.bound === 'maker' ? runs : undefined;
    if (boundRuns && !receiver.makerHolding) return failed('runs_unbound');
    const layout = this.layout({
      name: nameSlot,
      payload: claim.payload,
      handler: claim.handler,
      ack: claim.ack,
      base: claim.base,
      method: claim.method_key,
      runs: runs && !boundRuns ? { arg: runs.arg, ...(runs.key === undefined ? {} : { key: runs.key }) } : undefined,
    });
    if ('failure' in layout) return layout.failure;
    const callee = this.ownCallee(receiver, claim.path ?? [], claim.member, 'call');
    if ('failure' in callee) return callee.failure;
    const labels = claim.key_labels ?? {};
    let best: RankedFailure | undefined;
    let held = false;
    // An overload that holds but for a request member the declarations lack
    // (carrick#2968): the op stands without its request, unless another
    // overload holds the request too. Every other request fault fails the op.
    let requestDropped: string | undefined;
    const bodyMembers: ts.Declaration[] = [];
    for (const signature of callee.signatures) {
      let result = this.messageSignatureOutcome(signature, layout.value, labels);
      if (result.rank === Infinity && claim.yields !== undefined) {
        const given = this.yieldsOutcome(signature, claim.handler!, claim.yields, receiver);
        if (given) result = { rank: 5, outcome: given };
      }
      if (result.rank === Infinity && claim.request !== undefined) {
        const read = this.requestOutcome(signature, claim.handler!, claim.request, bodyMembers);
        if (read?.verdict === 'failed' && read.reason === 'request_member_missing') {
          requestDropped = read.reason;
          continue;
        }
        if (read) result = { rank: 5, outcome: read };
      }
      if (result.rank === Infinity) {
        held = true;
        requestDropped = undefined;
        break;
      }
      best = furthest(best, result.rank, result.outcome);
    }
    if (!held && requestDropped === undefined) return best!.outcome;
    if (boundRuns) {
      const ran = this.boundRunsOutcome(receiver.makerHolding!, boundRuns);
      return ran.verdict === 'verified' && requestDropped !== undefined ? { ...ran, requestDropped } : ran;
    }
    const declarations = this.declarationsOf(callee);
    const members = callee.named
      ? (callee.named.property.declarations ?? [])
      : callee.signatures.flatMap(signature => signature.getDeclaration() ?? []);
    return {
      verdict: 'verified',
      ...(declarations.length > 0 ? { declarations } : {}),
      ...(members.length > 0 ? { members } : {}),
      ...(bodyMembers.length > 0 ? { bodyMembers } : {}),
      ...(requestDropped === undefined ? {} : { requestDropped }),
    };
  }

  /**
   * Where a verified op or scope's member is declared, as position rows spell
   * it (`decl_file`, `decl_line`): the declaration the call row names for the
   * member symbol on this receiver ({@link memberDeclaration}), so two members
   * of one name on two types give two answers, and a property typed by an
   * interface with call signatures names the property, not the signatures
   * (carrick#2654). A callee that is the receiver itself has no member symbol
   * and names its signatures' declarations. A declaration no row could name is
   * left out.
   */
  private declarationsOf(callee: { signatures: readonly ts.Signature[]; named?: NamedMember }): DeclaredAt[] {
    const found: DeclaredAt[] = [];
    const named = callee.named
      ? [memberDeclaration(() => this.program, callee.named.property, callee.named.holder)]
      : callee.signatures.map(signature => signature.getDeclaration());
    for (const declaration of named) {
      if (!declaration) continue;
      const file = declaration.getSourceFile();
      const decl_file = declaringFileName(this.program, this.pathNames, file);
      if (decl_file === null) continue;
      found.push({ decl_file, decl_line: file.getLineAndCharacterOfPosition(declaration.getStart(file)).line + 1 });
    }
    return sortedDeclarations(found);
  }

  /**
   * The method fields of an op (label structure v2, carrick#2434), under the
   * role's method words. Without method words (the message roles), an op
   * carrying `method`, `method_key`, `options`, `method_default` or `base` is
   * `claim_invalid`, as before. With them:
   * - `method` is the member's own name, upper-cased (ASCII only), and a
   *   method word: `get` registers or sends GET (`method_not_member_verb`);
   * - `method_key` is where the method sits, a slot that accepts a string or
   *   a method literal (read with the call's other parts);
   * - `method_default` is a method word (`method_not_word`): beside
   *   `method_key`, the method when that option is absent; alone, the one
   *   method the call sends. Beside `method`, it says nothing new and is
   *   `claim_invalid`, as is `method` beside `method_key`;
   * - `options` belongs to the #1564 checks and is `claim_invalid`.
   * Undefined when the fields hold so far; their slots are read with the
   * call's other parts.
   */
  private methodOutcome(claim: OpClaim, rules: RoleRules): Outcome | undefined {
    const words = rules.methods;
    // A procedure named by its key (#2474 R6a): a receive op with a handler
    // whose label states its method, a route word. The member names the
    // procedure kind (`query`), not a verb, so it need not equal the method.
    if (claim.name !== undefined && 'bound' in claim.name && claim.name.bound === 'key') {
      if (!words || claim.op !== 'receive' || claim.handler === undefined || claim.method === undefined) {
        return unchecked('claim_invalid');
      }
      if (claim.method_key !== undefined || claim.method_default !== undefined || claim.options !== undefined) {
        return unchecked('claim_invalid');
      }
      return words.has(claim.method) ? undefined : failed('method_not_word');
    }
    if (!words) {
      const any =
        claim.method !== undefined ||
        claim.method_key !== undefined ||
        claim.options !== undefined ||
        claim.method_default !== undefined ||
        claim.base !== undefined;
      return any ? unchecked('claim_invalid') : undefined;
    }
    if (claim.options !== undefined) return unchecked('claim_invalid');
    if (claim.method !== undefined && (claim.method_key !== undefined || claim.method_default !== undefined)) {
      return unchecked('claim_invalid');
    }
    if (claim.method !== undefined) {
      if (claim.member === null || claim.method !== asciiUpperCase(claim.member) || !words.has(claim.method)) {
        return failed('method_not_member_verb');
      }
    }
    if (claim.method_default !== undefined && !words.has(claim.method_default)) return failed('method_not_word');
    return undefined;
  }

  /**
   * `runs: { bound: 'maker', arg, key }`: some overload the instance's maker
   * holds on takes a function with a declared signature there (the function
   * a mutation object's `mutate` runs is the one its maker was given).
   */
  private boundRunsOutcome(holding: readonly ts.Signature[], runs: RunsSlot): Outcome {
    let best: RankedFailure | undefined;
    for (const signature of holding) {
      let declared = this.keyParameterAt(signature, runs.arg);
      if (declared !== undefined && runs.key !== undefined) {
        const property = this.declaredProperty(declared, runs.key);
        if (!property) {
          best = furthest(best, 2, failed('key_missing'));
          continue;
        }
        declared = this.checker.getTypeOfSymbol(property);
      }
      const failure = this.runsFailure(declared);
      if (!failure) return VERIFIED;
      best = furthest(best, 3, failure);
    }
    return best?.outcome ?? failed('param_missing');
  }

  /** A `runs` slot: a function with a declared signature, as a handler is, under its own reasons. */
  private runsFailure(declared: Slot | undefined): Outcome | undefined {
    const failure = this.handlerFailure(declared);
    if (!failure || failure.verdict === 'verified') return failure;
    const reason = failure.reason.replace(/^handler_/, 'runs_');
    return failure.verdict === 'failed' ? failed(reason) : unchecked(reason);
  }

  /**
   * `yields: { param, export, side }` on a receive op or a scope: some
   * signature of the function this overload takes declares a parameter at
   * `param` whose type is either
   * - the package's own export named `export` (a class, or the interface an
   *   instance has), declared in the receiver's home packages; or
   * - when `export` is the maker whose own call made the instance the claim
   *   is read on (`madeBy`, carrick#2663): that instance, compared by
   *   declaration (`handsInstance`), since a factory's result has no export
   *   name of its own.
   * `side` is `client` or `server` (`yields_side_invalid`). Undefined when it
   * holds.
   */
  private yieldsOutcome(
    signature: ts.Signature,
    handler: ClaimSlot,
    yields: { param: number; export: string; side: string },
    receiver: MessageReceiver
  ): Outcome | undefined {
    if (yields.side !== 'client' && yields.side !== 'server') return failed('yields_side_invalid');
    let declared = this.keyParameterAt(signature, handler.arg);
    if (declared !== undefined && handler.key !== undefined) {
      const property = this.declaredProperty(declared, handler.key);
      declared = property ? this.checker.getTypeOfSymbol(property) : undefined;
    }
    if (declared === undefined) return failed('param_missing');
    const slot = this.throughConditional(this.throughConstraint(declared));
    if (slot === VARIADIC) return unchecked('handler_untyped');
    for (const part of this.parts(slot)) {
      for (const handed of part.getCallSignatures()) {
        const given = this.parameterAt(handed, yields.param);
        if (given === undefined || given === VARIADIC) continue;
        const named = typeSymbols(given).some(
          symbol =>
            symbol?.getName() === yields.export &&
            (symbol.declarations ?? []).some(declaration => this.isOwnFile(declaration.getSourceFile(), receiver.home))
        );
        if (named) return undefined;
        if (yields.export === receiver.madeBy && this.handsInstance(given, receiver, signature.typeParameters ?? [])) {
          return undefined;
        }
      }
    }
    return failed('yields_not_export');
  }

  /**
   * `given` (each part, nullish dropped) is the instance `receiver` holds:
   * some part of its intersection is a class or interface the receiver's
   * home packages declare, and the same declaration (the same symbol, never
   * the same name) as a part of the instance's own intersection. A generic
   * instance is built at its defaults while `given` threads the overload's
   * own type parameters (`own`) through its arguments, so the arguments are
   * read as `fitsErased` reads a mounted instance (carrick#2652): a
   * definite mismatch refuses, an argument it cannot read leaves the
   * declaration to decide.
   */
  private handsInstance(given: ts.Type, receiver: Holder, own: readonly ts.TypeParameter[]): boolean {
    const declared = (type: ts.Type): ts.Symbol | undefined => {
      const symbol = type.getSymbol();
      if (!symbol || !(symbol.flags & (ts.SymbolFlags.Class | ts.SymbolFlags.Interface))) return undefined;
      const home = (symbol.declarations ?? []).some(declaration => this.isOwnFile(declaration.getSourceFile(), receiver.home));
      return home ? symbol : undefined;
    };
    const intersected = (type: ts.Type): readonly ts.Type[] => (type.isIntersection() ? type.types : [type]);
    const instances = this.parts(receiver.type)
      .flatMap(intersected)
      .filter(part => declared(part) !== undefined);
    const parts = this.parts(given);
    return (
      parts.length > 0 &&
      parts.every(part =>
        intersected(part).some(handed => {
          const symbol = declared(handed);
          return (
            symbol !== undefined &&
            instances.some(instance => declared(instance) === symbol && this.fitsErased(instance, handed, own) !== false)
          );
        })
      )
    );
  }

  /**
   * `request: { param, path?, url?, method?, body? }` on a receive op (#2474
   * R0, carrick#2674): some signature of the handler this overload takes
   * declares a parameter at `param` on which every member path the claim
   * gives resolves, step by step, through properties the library declares
   * (`declaredProperty`). Undefined when it holds; the declarations of the
   * member the `body` path ends on are then added to `bodyMembers`.
   */
  private requestOutcome(
    signature: ts.Signature,
    handler: ClaimSlot,
    request: RequestAt,
    bodyMembers: ts.Declaration[]
  ): Outcome | undefined {
    let declared = this.keyParameterAt(signature, handler.arg);
    if (declared !== undefined && handler.key !== undefined) {
      const property = this.declaredProperty(declared, handler.key);
      declared = property ? this.checker.getTypeOfSymbol(property) : undefined;
    }
    if (declared === undefined) return failed('param_missing');
    const slot = this.throughConditional(this.throughConstraint(declared));
    if (slot === VARIADIC) return unchecked('handler_untyped');
    const paths = [request.path, request.url, request.method, request.body].filter(
      (p): p is string[] => p !== undefined
    );
    let best: Outcome = failed('param_missing');
    for (const part of this.parts(slot)) {
      for (const handed of part.getCallSignatures()) {
        const given = this.parameterAt(handed, request.param);
        if (given === undefined || given === VARIADIC) continue;
        const ends = new Map<string[], ts.Symbol>();
        const resolves = paths.every(members => {
          let at: Slot = given;
          let last: ts.Symbol | undefined;
          for (const member of members) {
            const property = this.declaredProperty(at, member);
            if (!property) return false;
            last = property;
            at = this.checker.getTypeOfSymbol(property);
          }
          if (last) ends.set(members, last);
          return true;
        });
        if (resolves) {
          const body = request.body ? ends.get(request.body) : undefined;
          bodyMembers.push(...(body?.declarations ?? []));
          return undefined;
        }
        best = failed('request_member_missing');
      }
    }
    return best;
  }

  /**
   * The declarations spell `name` in one of `member`'s parameters: an
   * overload whose parameter there is that literal, or a key of the map a
   * `keyof` constraint reads. The claim carries no slot, so every parameter
   * is read. A name the library emits itself only ever removes rows, so this
   * verdict is for the record. The runtime's own emitter members also
   * reserve the names the runtime gives every emitter, which no declaration
   * states (`RUNTIME_EMITTER_EVENTS`, carrick#3032).
   */
  private judgeReserved(receiver: MessageReceiver, claim: ReservedClaim): Outcome {
    const callee = this.ownCallee(receiver, claim.path ?? [], claim.member, 'call');
    if ('failure' in callee) return callee.failure;
    for (const signature of callee.signatures) {
      const count = signature.getParameters().length;
      for (let arg = 0; arg < count; arg++) {
        if (this.literalsAt(signature, { arg }, callee.holder).has(claim.name)) return VERIFIED;
      }
    }
    if (RUNTIME_EMITTER_EVENTS.includes(claim.name) && this.isRuntimeEmitterProperty(callee.named?.property)) return VERIFIED;
    return failed('reserved_not_declared');
  }

  /** The property is a member of the runtime's own emitter (`isRuntimeEmitterMember`). */
  private isRuntimeEmitterProperty(property: ts.Symbol | undefined): boolean {
    return property !== undefined && isRuntimeEmitterMember(property, file => this.packageOf(file));
  }

  /**
   * Where a claim's parts sit. Two parts at one position (`send(data)` read
   * as both the name and the payload), or a part at a position another part
   * reads keys of, describe no call: `slots_overlap`.
   */
  private layout(parts: Partial<Record<PartName, ClaimSlot | undefined>>): { value: ClaimLayout } | { failure: Outcome } {
    const positional = new Map<number, PartName>();
    const keyed = new Map<number, Map<string, PartName>>();
    for (const [part, slot] of Object.entries(parts) as [PartName, ClaimSlot | undefined][]) {
      if (slot === undefined) continue;
      if (slot.key === undefined) {
        if (positional.has(slot.arg) || keyed.has(slot.arg)) return { failure: failed('slots_overlap') };
        positional.set(slot.arg, part);
        continue;
      }
      if (positional.has(slot.arg)) return { failure: failed('slots_overlap') };
      const keys = keyed.get(slot.arg) ?? new Map<string, PartName>();
      if (keys.has(slot.key)) return { failure: failed('slots_overlap') };
      keys.set(slot.key, part);
      keyed.set(slot.arg, keys);
    }
    return { value: { positional, keyed } };
  }

  /**
   * One overload against a message claim's layout; rank `Infinity` holds.
   *
   * 1. A positional name, base or prefix accepts a string.
   * 2. A positional name is not a key of an index-signature map; a payload
   *    is there (its type is not read) and is not a callback.
   * 3. A positional handler or acknowledgement is a function type with a
   *    declared signature. Keyed parts: the object at that argument declares
   *    every claimed key in one view (one union member), a name, base or
   *    prefix key accepting a string and a handler key a declared function.
   * 4. Every string slot of the call besides the name is accounted for
   *    (design D2, `nameSiblings`).
   *
   * `runsAs` replaces the `runs` predicate (an adapter's collection, #2474
   * R6a); absent, a `runs` slot is a declared function.
   */
  private messageSignatureOutcome(
    signature: ts.Signature,
    layout: ClaimLayout,
    labels: KeyLabels,
    runsAs?: RunsPredicate
  ): RankedFailure {
    const runsFailure = runsAs ?? ((declared: Slot | undefined) => this.runsFailure(declared));
    for (const [arg, part] of layout.positional) {
      if (part === 'method') {
        const declared = this.keyParameterAt(signature, arg);
        if (declared === undefined) return { rank: 1, outcome: failed('param_missing') };
        if (!this.acceptsMethod(declared)) return { rank: 1, outcome: this.slotFailure(declared, 'method_not_string') };
        continue;
      }
      if (part !== 'name' && part !== 'base' && part !== 'prefix') continue;
      const declared = this.keyParameterAt(signature, arg);
      if (declared === undefined) return { rank: 1, outcome: failed('param_missing') };
      const slot = this.throughConditional(declared);
      if (!this.acceptsString(slot)) {
        return { rank: 1, outcome: this.slotFailure(slot, part === 'name' ? 'name_not_string' : 'slot_not_string') };
      }
    }
    for (const [arg, part] of layout.positional) {
      if (part === 'name' && this.isIndexKeySlot(signature, arg)) {
        return { rank: 2, outcome: failed('name_index_key') };
      }
      if (part === 'payload') {
        const payload = this.parameterAt(signature, arg);
        if (payload === undefined) return { rank: 2, outcome: failed('payload_missing') };
        // Function versus data is shape: a callback in that place is not the payload.
        if (this.isFunctionSlot(payload)) return { rank: 2, outcome: failed('payload_is_function') };
      }
    }
    for (const [arg, part] of layout.positional) {
      if (part !== 'handler' && part !== 'ack' && part !== 'runs') continue;
      const declared = this.keyParameterAt(signature, arg);
      const failure = part === 'runs' ? runsFailure(declared) : this.handlerFailure(declared);
      if (failure) return { rank: 3, outcome: failure };
    }
    const views = new Map<number, ts.Type>();
    for (const [arg, keys] of layout.keyed) {
      const slot = this.keyParameterAt(signature, arg);
      if (slot === undefined) return { rank: 1, outcome: failed('param_missing') };
      const view = this.viewFor(slot, keys, runsFailure);
      if ('rank' in view) return view;
      views.set(arg, view.view);
    }
    const ambiguity = this.nameSiblings(signature, layout, views, labels);
    if (ambiguity) return { rank: 4, outcome: ambiguity };
    return { rank: Infinity, outcome: VERIFIED };
  }

  /**
   * The object at a keyed argument, read as one view: the whole type, or one
   * union member that is an object type and not a function type, declaring
   * every claimed key with the type its part needs.
   */
  private viewFor(slot: Slot, keys: ReadonlyMap<string, PartName>, runsFailure: RunsPredicate): { view: ts.Type } | RankedFailure {
    if (slot === VARIADIC || !this.isObjectType(slot)) {
      return { rank: 1, outcome: this.slotFailure(slot, 'param_missing') };
    }
    const views = this.keyViews(slot);
    let best: RankedFailure | undefined;
    if (views.length === 0) best = { rank: 2, outcome: failed('key_missing') };
    for (const view of views) {
      let failure: RankedFailure | undefined;
      for (const [key, part] of keys) {
        const property = this.declaredProperty(view, key);
        if (!property) {
          failure = { rank: 2, outcome: failed('key_missing') };
          break;
        }
        const type = this.checker.getTypeOfSymbol(property);
        if ((part === 'name' || part === 'base' || part === 'prefix') && !this.acceptsString(this.throughConditional(type))) {
          failure = { rank: 3, outcome: this.slotFailure(this.throughConditional(type), 'key_not_string') };
          break;
        }
        if (part === 'payload' && this.isFunctionSlot(type)) {
          failure = { rank: 3, outcome: failed('payload_is_function') };
          break;
        }
        if (part === 'method' && !this.acceptsMethod(type)) {
          failure = { rank: 3, outcome: this.slotFailure(type, 'method_not_string') };
          break;
        }
        if (part === 'runs') {
          const bad = runsFailure(type);
          if (bad) {
            failure = { rank: 3, outcome: bad };
            break;
          }
        }
        if (part === 'handler' || part === 'ack') {
          const bad = this.handlerFailure(type);
          if (bad) {
            failure = { rank: 3, outcome: bad };
            break;
          }
        }
      }
      if (!failure) return { view };
      best = furthest(best, failure.rank, failure.outcome);
    }
    return best!;
  }

  /**
   * The objects a claim's keys at one argument are read from, one at a time:
   * the whole type, or each union member that is an object type and not a
   * function type.
   */
  private keyViews(slot: ts.Type): readonly ts.Type[] {
    const parts = this.parts(slot);
    return parts.length > 1 ? parts.filter(part => this.isPlainObject(part)) : [slot];
  }

  /**
   * Strict D2: `name_ambiguous` unless every string slot of the call besides
   * the name is accounted for, because which of two string slots is the name
   * is behaviour, not shape. A slot is accounted for when the claim assigns it
   * a part (payload, handler, base, prefix), or, for a key of an object at
   * the call, when `key_labels` labels it `not_name`. A positional string the
   * claim leaves unassigned cannot be labelled, so it always competes. A
   * slot or key counts as a string exactly when a name there would
   * (`siblingTakesString`): a conditional counts when one of its branches
   * takes a string (fail closed, carrick#1687).
   *
   * The labels must agree with the claim: at most one key is labelled `name`,
   * and only the claim's own name key; that key is never labelled `not_name`.
   * A `not_name` label for a key an overload does not declare is ignored
   * (one label map serves every overload). No name, no rule.
   *
   * Rest parameters: a rest the claim puts a payload or handler in belongs
   * wholly to that part; one that holds the name holds other names too; from
   * the first variable element of a tuple rest (`[...channels: string[], cb]`)
   * no position is fixed, so a name there is one of many. A rest the claim
   * leaves unassigned competes when its element accepts a string, or when it
   * cannot be read at all (`...args: Rest<B>`, a conditional on a type
   * parameter), since it could hold a string slot.
   */
  private nameSiblings(
    signature: ts.Signature,
    layout: ClaimLayout,
    views: ReadonlyMap<number, ts.Type>,
    labels: KeyLabels
  ): Outcome | undefined {
    const namedAt = [...layout.positional].find(([, part]) => part === 'name')?.[0];
    let nameKey: string | undefined;
    for (const keys of layout.keyed.values()) {
      for (const [key, part] of keys) if (part === 'name') nameKey = key;
    }
    if (namedAt === undefined && nameKey === undefined) return undefined;
    const ambiguous = failed('name_ambiguous');

    const nameLabels = Object.keys(labels).filter(key => labels[key] === 'name');
    if (nameLabels.length > 1) return ambiguous;
    if (nameLabels.length === 1 && nameLabels[0] !== nameKey) return ambiguous;
    if (nameKey !== undefined && labels[nameKey] === 'not_name') return ambiguous;

    // A string key at the call the claim neither assigns nor labels.
    const unaccountedKey = (keys: readonly string[], assigned: ReadonlyMap<string, PartName>) =>
      keys.some(key => !assigned.has(key) && labels[key] !== 'not_name');
    const none = new Map<string, PartName>();
    const takesString = (slot: Slot | undefined) => slot !== undefined && this.siblingTakesString(slot);
    // An argument the claim gives no part: a string, an object with a string
    // key nobody accounts for, or a rest the verifier cannot read, which could
    // hold either and no label can account for (fail closed, carrick#1687).
    const competes = (slot: Slot | undefined) =>
      slot === VARIADIC || takesString(slot) || unaccountedKey(this.stringKeys(slot), none);

    // Every object the claim reads keys of: its other string keys.
    for (const [arg, keys] of layout.keyed) {
      const view = views.get(arg);
      if (view && unaccountedKey(this.stringKeys(view), keys)) return ambiguous;
    }

    const parameters = signature.getParameters();
    const last = parameters[parameters.length - 1];
    const lastDeclaration = last?.valueDeclaration;
    const restIndex =
      lastDeclaration && ts.isParameter(lastDeclaration) && lastDeclaration.dotDotDotToken ? parameters.length - 1 : -1;
    const fixed = restIndex === -1 ? parameters.length : restIndex;
    const assigned = (i: number) => layout.positional.has(i) || layout.keyed.has(i);

    for (let i = 0; i < fixed; i++) {
      if (assigned(i)) continue;
      if (competes(this.keyParameterAt(signature, i))) return ambiguous;
    }
    if (restIndex === -1) return undefined;
    const rest = this.checker.getTypeOfSymbol(last);
    if (this.checker.isTupleType(rest)) {
      const elements = this.checker.getTypeArguments(rest as ts.TypeReference);
      const flags = ((rest as ts.TypeReference).target as ts.TupleType).elementFlags;
      const variable = flags.findIndex(flag => (flag & ts.ElementFlags.Variable) !== 0);
      const fixedLength = variable === -1 ? elements.length : variable;
      for (let i = restIndex; i < restIndex + fixedLength; i++) {
        if (assigned(i)) continue;
        if (competes(this.keyParameterAt(signature, i))) return ambiguous;
      }
      if (variable !== -1) {
        const unfixed = [...layout.positional].filter(([arg]) => arg >= restIndex + variable).map(([, part]) => part);
        if (unfixed.includes('name')) return ambiguous;
        const element = elements[variable];
        const each =
          element !== undefined && this.checker.isArrayType(element)
            ? this.checker.getTypeArguments(element as ts.TypeReference)[0]
            : element;
        if (unfixed.length === 0 && competes(each === undefined ? undefined : this.throughConstraint(each))) {
          return ambiguous;
        }
      }
      return undefined;
    }
    const inRest = [...layout.positional].filter(([arg]) => arg >= restIndex).map(([, part]) => part);
    if (inRest.includes('name')) return takesString(this.keyParameterAt(signature, restIndex)) ? ambiguous : undefined;
    // A rest the claim puts a payload or handler in is that part's.
    if (inRest.length > 0) return undefined;
    return competes(this.keyParameterAt(signature, restIndex)) ? ambiguous : undefined;
  }

  /**
   * A slot beside the name takes a string, read as a name there is read: a
   * conditional through its branches (`throughConditional`). One whose
   * branch says nothing counts no more than `any` does.
   */
  private siblingTakesString(slot: Slot): boolean {
    return this.acceptsString(this.throughConditional(slot));
  }

  /**
   * The string-accepting keys of an argument: every key some object part of
   * it declares, read through a type parameter's constraint.
   */
  private stringKeys(slot: Slot | undefined): string[] {
    if (slot === undefined || slot === VARIADIC) return [];
    const keys = new Set<string>();
    for (const part of this.parts(this.throughConstraint(slot) as ts.Type)) {
      if (!this.isObjectLike(part)) continue;
      for (const property of this.namedProperties(part)) {
        if (this.siblingTakesString(this.checker.getTypeOfSymbol(property))) keys.add(property.getName());
      }
    }
    return [...keys];
  }

  /**
   * The parameter at `index` is declared as a key of a map with an index
   * signature: `keyof M`, or a type parameter constrained by one, where `M`
   * (or its constraint) declares `[key: string]: ...`. Such a slot takes any
   * key the map could hold, so it says nothing about a name. Read at the
   * declaration, where the map is still the type parameter the library wrote.
   */
  private isIndexKeySlot(signature: ts.Signature, index: number): boolean {
    const declaration = signature.getDeclaration();
    if (!declaration) return false;
    const parameters = declaration.parameters;
    const parameter = parameters[Math.min(index, parameters.length - 1)];
    if (!parameter?.type) return false;
    if (index >= parameters.length && !parameter.dotDotDotToken) return false;
    return this.keysOfIndexMap(this.checker.getTypeFromTypeNode(parameter.type), parameter.type, 0);
  }

  private keysOfIndexMap(type: ts.Type, node: ts.TypeNode | undefined, depth: number): boolean {
    if (depth > 6) return false;
    // `keyof M` written against a concrete map is resolved at once to the
    // map's keys (`string | number` for an index signature); the node keeps
    // what was written.
    if (node && ts.isTypeOperatorNode(node) && node.operator === ts.SyntaxKind.KeyOfKeyword) {
      return this.isIndexMap(this.checker.getTypeFromTypeNode(node.type));
    }
    if (type.flags & ts.TypeFlags.Index) {
      return this.isIndexMap((type as ts.IndexType).type);
    }
    if (type.flags & ts.TypeFlags.TypeParameter) {
      const constraintNode = this.declaredConstraint(type);
      return (
        constraintNode !== undefined &&
        this.keysOfIndexMap(this.checker.getTypeFromTypeNode(constraintNode), constraintNode, depth + 1)
      );
    }
    if (type.isUnionOrIntersection()) {
      return type.types.some(part => this.keysOfIndexMap(part, undefined, depth + 1));
    }
    return false;
  }

  /** The constraint a type parameter's declaration writes, unresolved (`K extends keyof M`). */
  private declaredConstraint(type: ts.Type): ts.TypeNode | undefined {
    return (type.getSymbol()?.declarations ?? []).find(ts.isTypeParameterDeclaration)?.constraint;
  }

  /**
   * The map a `keyof` reads is a concrete map with an index signature. A map
   * the library takes as a type parameter (an event map defaulting to an
   * index signature) is the service's to type: its own type argument only
   * narrows which names the slot allows and never moves the name to another
   * argument, so the key slot reads as a string slot (contract amendment 2,
   * B6, the `index_key_generic_map` reading, now the only one).
   */
  private isIndexMap(map: ts.Type): boolean {
    if (map.flags & ts.TypeFlags.TypeParameter) return false;
    return this.hasIndexSignature(map);
  }

  private hasIndexSignature(type: ts.Type): boolean {
    const target =
      type.flags & ts.TypeFlags.TypeParameter ? this.checker.getBaseConstraintOfType(type) ?? type : type;
    return this.checker.getIndexInfosOfType(this.checker.getApparentType(target)).length > 0;
  }

  /**
   * A handler or acknowledgement slot: every part besides null and undefined
   * is a function type with a declared signature. `Function`, `any`,
   * `unknown`, an unconstrained type parameter and `(...args: any[])` say
   * nothing about a handler (`handler_untyped`); a part that is not a
   * function (an options object, a string) is `handler_not_function`.
   */
  private handlerFailure(declared: Slot | undefined): Outcome | undefined {
    if (declared === undefined) return failed('param_missing');
    // A listener typed by deferred conditional machinery (`ListenerOf<Ev>`)
    // is read through its branches; one that says nothing is untyped.
    const slot = this.throughConditional(this.throughConstraint(declared));
    if (slot === VARIADIC) return unchecked('handler_untyped');
    const parts = this.parts(slot);
    if (parts.length === 0) return failed('handler_not_function');
    for (const part of parts) {
      if (this.isOpenTop(part) || this.isUnconstrained(part)) return unchecked('handler_untyped');
      const signatures = part.getCallSignatures();
      if (signatures.length === 0) {
        return this.isEmptyObject(part) ? unchecked('handler_untyped') : failed('handler_not_function');
      }
      if (signatures.every(signature => this.isUntypedSignature(signature))) return unchecked('handler_untyped');
    }
    return undefined;
  }

  /**
   * A name typed by a conditional (`IdOf<T> = T extends Def<infer Id> ? Id
   * : never`) is read through what its branches allow. When a branch says
   * nothing (`... ? Id : any`), so does the slot: the checker's constraint of
   * such a conditional drops the `any` branch and would read as a string.
   */
  private throughConditional(slot: Slot): Slot {
    if (slot === VARIADIC) return slot;
    const parts = this.parts(slot);
    if (parts.length !== 1 || !(parts[0].flags & ts.TypeFlags.Conditional)) return slot;
    const node = (parts[0] as ts.ConditionalType).root.node;
    const branchSaysNothing = [node.trueType, node.falseType].some(branch => {
      const type = this.checker.getTypeFromTypeNode(branch);
      return (type.flags & ts.TypeFlags.Never) === 0 && this.returnSaysNothing(type);
    });
    if (branchSaysNothing) return VARIADIC;
    return this.checker.getBaseConstraintOfType(parts[0]) ?? slot;
  }

  /** Every part besides null and undefined is callable: a function, not data. */
  private isFunctionSlot(declared: Slot): boolean {
    const slot = this.throughConstraint(declared);
    if (slot === VARIADIC) return false;
    const parts = this.parts(slot);
    return parts.length > 0 && parts.every(part => !this.isOpenTop(part) && part.getCallSignatures().length > 0);
  }

  /** `(...args: any[])` or `(...args: unknown[])`: a signature that declares nothing. */
  private isUntypedSignature(signature: ts.Signature): boolean {
    const parameters = signature.getParameters();
    if (parameters.length !== 1) return false;
    const declaration = parameters[0].valueDeclaration;
    if (!declaration || !ts.isParameter(declaration) || !declaration.dotDotDotToken) return false;
    const rest = this.checker.getTypeOfSymbol(parameters[0]);
    if (this.isOpenTop(rest)) return true;
    return this.checker.isArrayType(rest) && this.isOpenTop(this.checker.getTypeArguments(rest as ts.TypeReference)[0]);
  }

  /**
   * The string literals a slot spells: the literal parts of its type, through
   * a type parameter's constraint, and the property names of a map a `keyof`
   * reads, both as instantiated and as written at the declaration. `holder`
   * is the type the member was read from: a map that is a type parameter of
   * a base class is read as the holder's class chain binds it.
   */
  private literalsAt(signature: ts.Signature, at: ClaimSlot, holder: ts.Type): ReadonlySet<string> {
    const literals = new Set<string>();
    let slot: Slot | undefined = this.parameterAt(signature, at.arg);
    if (slot !== undefined && slot !== VARIADIC && at.key !== undefined) {
      const property = this.declaredProperty(slot, at.key);
      slot = property ? this.checker.getTypeOfSymbol(property) : undefined;
    }
    if (slot !== undefined && slot !== VARIADIC) this.spell(slot, literals, 0, holder);
    if (at.key === undefined) {
      const parameters = signature.getDeclaration()?.parameters;
      const node = parameters?.[Math.min(at.arg, parameters.length - 1)]?.type;
      if (node) this.spell(this.checker.getTypeFromTypeNode(node), literals, 0, holder);
    }
    return literals;
  }

  private spell(type: ts.Type, into: Set<string>, depth: number, holder: ts.Type): void {
    if (depth > 6) return;
    if (type.isStringLiteral()) {
      into.add(type.value);
      return;
    }
    if (type.isUnionOrIntersection()) {
      for (const part of type.types) this.spell(part, into, depth + 1, holder);
      return;
    }
    if (type.flags & ts.TypeFlags.TypeParameter) {
      const constraintNode = this.declaredConstraint(type);
      this.spell(
        constraintNode ? this.checker.getTypeFromTypeNode(constraintNode) : this.checker.getBaseConstraintOfType(type) ?? type,
        into,
        depth + 1,
        holder
      );
      return;
    }
    if (type.flags & ts.TypeFlags.Index) {
      let map = (type as ts.IndexType).type;
      if (map.flags & ts.TypeFlags.TypeParameter) map = this.heritageArgument(map, holder);
      if (map.flags & ts.TypeFlags.TypeParameter) {
        map = this.checker.getDefaultFromTypeParameter(map) ?? this.checker.getBaseConstraintOfType(map) ?? map;
      }
      for (const property of this.checker.getPropertiesOfType(this.checker.getApparentType(map))) {
        if (!isSymbolKeyed(property)) into.add(property.getName());
      }
    }
  }

  /**
   * The type argument a class's type parameter takes in `holder`'s class
   * chain: the argument a heritage clause passes for it
   * (`class Channel<L> extends Emitter<L, Reserved>` binds Emitter's second
   * parameter to `Reserved`), followed down the chain while that argument is
   * a type parameter of a class below. A parameter no heritage clause binds
   * comes back as it is (or as the last parameter it was bound to), for the
   * caller to read at its default.
   */
  private heritageArgument(parameter: ts.Type, holder: ts.Type): ts.Type {
    const bases: ts.TypeReference[] = [];
    const seen = new Set<ts.Type>();
    const visit = (type: ts.Type, depth: number): void => {
      if (depth > 8) return;
      if (type.isIntersection()) {
        for (const part of type.types) visit(part, depth + 1);
        return;
      }
      const target = ((type as ts.ObjectType).objectFlags ?? 0) & ts.ObjectFlags.Reference ? (type as ts.TypeReference).target : type;
      if (!(((target as ts.ObjectType).objectFlags ?? 0) & ts.ObjectFlags.ClassOrInterface) || seen.has(target)) return;
      seen.add(target);
      for (const base of this.checker.getBaseTypes(target as ts.InterfaceType)) {
        if (((base as ts.ObjectType).objectFlags ?? 0) & ts.ObjectFlags.Reference) bases.push(base as ts.TypeReference);
        visit(base, depth + 1);
      }
    };
    visit(this.checker.getNonNullableType(holder), 0);
    let bound = parameter;
    for (let step = 0; step <= bases.length && bound.flags & ts.TypeFlags.TypeParameter; step++) {
      const base = bases.find(each => (each.target.typeParameters ?? []).includes(bound as ts.TypeParameter));
      if (!base) break;
      const argument = this.checker.getTypeArguments(base)[base.target.typeParameters!.indexOf(bound as ts.TypeParameter)];
      if (!argument) break;
      bound = argument;
    }
    return bound;
  }

  /**
   * The callee of a message claim: the claim's member `path` walked from the
   * receiver hop by hop (`client.tasks.trigger`), then `member` of the object
   * reached (or that object itself, when null), called (`call`) or
   * constructed (`new`), through the signatures its home packages declare.
   * A member, or every signature, that only another package declares (the
   * runtime's event emitter a library class extends, a base class from a
   * dependency) is `member_inherited`. Each hop must be a home member too;
   * the object it reaches adds the packages that declare its type to the
   * home, never the runtime's, and a hop whose type says nothing has no
   * members to read (`member_untyped`).
   */
  private ownCallee(
    receiver: MessageReceiver,
    hops: readonly string[],
    member: string | null,
    form: 'call' | 'new'
  ): Callable {
    let holder: Holder = receiver;
    // The last member the path reaches, with the type it is read on: the
    // symbol the call row names (carrick#2654).
    let named: NamedMember | undefined;
    for (const hop of hops) {
      const step = this.ownMember(holder, hop);
      if ('failure' in step) return step;
      const type = step.type;
      if (this.saysNothing(type)) return { failure: unchecked('member_untyped') };
      named = { property: step.property, holder: holder.type };
      holder = { type, home: this.homeOf(receiver.pkg, typeSymbols(type), holder.home) };
    }
    let type = holder.type;
    // A member of a base the holder binds with its own type (see `isBoundByOwnType`).
    let boundBase = false;
    if (member !== null) {
      const step = this.ownMember(holder, member);
      if ('failure' in step) return step;
      type = step.type;
      boundBase = step.bound;
      named = { property: step.property, holder: holder.type };
    }
    const all =
      form === 'new'
        ? this.checker.getNonNullableType(type).getConstructSignatures()
        : this.checker.getNonNullableType(type).getCallSignatures();
    const library = all.filter(signature => {
      const file = this.signatureFile(signature, type, form);
      return file !== undefined && this.isLibraryFile(file);
    });
    const home = holder.home;
    const own = boundBase
      ? library
      : library.filter(signature => this.isOwnFile(this.signatureFile(signature, type, form)!, home));
    if (own.length > 0) return { signatures: own, holder: holder.type, named };
    if (library.length > 0) return { failure: failed('member_inherited') };
    return { failure: this.slotFailure(type, form === 'new' ? 'member_not_constructible' : 'member_not_callable') };
  }

  /**
   * Member `name` of `holder`, when its home packages declare it, or when it
   * sits on another package's base that the holder binds with its own type
   * (`bound`).
   */
  private ownMember(
    holder: Holder,
    name: string
  ): { type: ts.Type; bound: boolean; property: ts.Symbol } | { failure: Outcome } {
    const apparent = this.checker.getApparentType(this.checker.getNonNullableType(holder.type));
    const property = this.declaredProperty(holder.type, name);
    if (!property) return { failure: failed('member_missing') };
    const type = this.checker.getTypeOfSymbol(property);
    if (this.isOwnMember(property, apparent, holder.home)) return { type, bound: false, property };
    if (this.isBoundByOwnType(property, holder)) return { type, bound: true, property };
    return { failure: failed('member_inherited') };
  }

  /**
   * An inherited emitter the client binds to its own interface counts as
   * declared: the member is declared on a base type another package writes,
   * and the holder's class binds that base with at least one concrete type
   * argument (not a type parameter) its home packages declare:
   * `class Socket<L, E> extends Emitter<L, E, SocketReservedEvents>`. The
   * runtime's emitter, extended with no type of the package's own, is not
   * bound; nor is a base bound only through the class's type parameters.
   */
  private isBoundByOwnType(property: ts.Symbol, holder: Holder): boolean {
    const owners = (property.declarations ?? [])
      .map(declaration => declaration.parent)
      .filter((owner): owner is ts.ClassLikeDeclaration | ts.InterfaceDeclaration =>
        owner !== undefined && (ts.isClassLike(owner) || ts.isInterfaceDeclaration(owner))
      );
    if (owners.length === 0) return false;
    const targetOf = (type: ts.Type): ts.Type =>
      ((type as ts.ObjectType).objectFlags ?? 0) & ts.ObjectFlags.Reference ? (type as ts.TypeReference).target : type;
    const isOwnType = (type: ts.Type) =>
      !(type.flags & ts.TypeFlags.TypeParameter) &&
      typeSymbols(type).some(symbol =>
        (symbol?.declarations ?? []).some(declaration => this.isOwnFile(declaration.getSourceFile(), holder.home))
      );
    const visit = (type: ts.Type, depth: number): boolean => {
      if (depth > 8) return false;
      if (type.isIntersection()) return type.types.some(part => visit(part, depth + 1));
      const target = targetOf(type);
      if (!(((target as ts.ObjectType).objectFlags ?? 0) & ts.ObjectFlags.ClassOrInterface)) return false;
      for (const base of this.checker.getBaseTypes(target as ts.InterfaceType)) {
        const declarations = targetOf(base).getSymbol()?.declarations ?? [];
        if (owners.some(owner => declarations.includes(owner))) {
          const bound =
            ((base as ts.ObjectType).objectFlags ?? 0) & ts.ObjectFlags.Reference
              ? this.checker.getTypeArguments(base as ts.TypeReference)
              : [];
          if (bound.some(isOwnType)) return true;
          continue;
        }
        if (visit(base, depth + 1)) return true;
      }
      return false;
    };
    return visit(this.checker.getNonNullableType(holder.type), 0);
  }

  /**
   * The file that wrote a signature. A class's construct signatures are the
   * class's own, wherever the constructor they reuse was written: a class
   * with no constructor of its own is built through its base's (or a default
   * one with no declaration at all), and still builds an instance of itself.
   */
  private signatureFile(signature: ts.Signature, callee: ts.Type, form: 'call' | 'new'): ts.SourceFile | undefined {
    if (form === 'new') {
      const declaration = callee.getSymbol()?.declarations?.find(ts.isClassLike);
      if (declaration) return declaration.getSourceFile();
    }
    return signature.getDeclaration()?.getSourceFile();
  }

  /**
   * Some declaration of the member is in one of the receiver's own packages.
   * A member a mapped type makes has no declaration of its own; it is own when
   * it reaches the receiver along types the own packages (or the default
   * library's utility types, `Record`) declare, and the service's own blocks
   * do not name it.
   */
  private isOwnMember(property: ts.Symbol, listing: ts.Type, home: ReadonlySet<string>): boolean {
    const declarations = property.declarations ?? [];
    if (declarations.length === 0) {
      return (
        !this.isNamedByServiceAugmentation(property.getName()) &&
        this.isListedByLibraryType(
          listing,
          property.getName(),
          file => this.program.isSourceFileDefaultLibrary(file) || this.isOwnFile(file, home)
        )
      );
    }
    return declarations.some(declaration => this.isOwnFile(declaration.getSourceFile(), home));
  }

  // --------------------------------------------------------------------------
  // Surface listing (carrick#1660)
  // --------------------------------------------------------------------------

  /**
   * One specifier's declared surface (see `LibraryClaimsVerifier.listSurface`).
   * `built` answers the probe's no-argument build of a receiver an export
   * makes, resolved, when the probe holds one.
   */
  listPackage(
    pkg: string,
    probe: SurfaceProbe,
    maxEntries: number,
    only: readonly string[] | undefined,
    options: SurfaceOptions,
    typeExports: readonly BuiltType[],
    built: (exportName: string, receiver: string) => ts.Signature | undefined
  ): LibrarySurface {
    this.setPackage(pkg);
    // Runtime modules on: a `node:` specifier is listed from the runtime's
    // types package, as the message roles read it.
    const moduleRead =
      'defaultLibrary' in probe ? this.readDefaultLibrary(pkg) : this.readModule(pkg, probe.namespace, true);
    const surface: LibrarySurface = { package: pkg, truncated: 0, exports: [] };
    if (moduleRead.entry.resolved_file) surface.resolved_file = moduleRead.entry.resolved_file;
    if (moduleRead.entry.installed_version) surface.installed_version = moduleRead.entry.installed_version;
    if (moduleRead.reason) return { ...surface, reason: moduleRead.reason };
    const exported = this.listedExports(probe, only, moduleRead.globalFile);
    if (!exported) return { ...surface, reason: 'module_unresolved' };

    let budget = maxEntries;
    let dropped = 0;
    const take = (): boolean => {
      if (budget > 0) {
        budget -= 1;
        return true;
      }
      dropped += 1;
      return false;
    };
    // Handler-parameter members (carrick#2674) take nothing from the main cap,
    // so the listing, `truncated` and the hash are the ones the lister gave
    // before handlers were listed; each parameter type has a cap of its own
    // (`HANDLER_MEMBER_CAP`, carrick#2966), and is listed once per surface in
    // `handler_types` (carrick#2977).
    this.listResultCandidates = options.resultCandidates === true;
    const withHandlers = options.handlers === true;
    this.listTypes = options.types === true;
    this.collectTypes = this.listTypes;
    this.typeCandidates.clear();
    this.typeIds.clear();
    this.typesLeftOut = 0;
    this.typeContext = '';
    this.specifierHome = this.homeOf(pkg, []);
    this.exportTargets = new Map(exported.map(each => [each.target, each.name] as const).reverse());
    if (withHandlers || this.listTypes) {
      this.handlerTypes = [];
      this.handlerTypeIds.clear();
      this.handlerTypeByContent.clear();
      this.handlerParamsLeftOut = 0;
      this.typeExportNames = new Map();
      for (const each of [...typeExports].sort((a, b) => byName(a.name, b.name))) {
        if (!this.typeExportNames.has(each.symbol)) this.typeExportNames.set(each.symbol, each.name);
      }
    }
    // Four passes: every export and receiver, then every member name, then the
    // members of each handler's parameters (carrick#2674; only when asked,
    // carrick#2966), then signatures.
    const names: Array<() => void> = [];
    const handlers: Array<() => void> = [];
    const fills: Array<() => void> = [];

    // What the package's default export names: a maker also exported by name
    // under this declaration is imported the same way, so it is read the same.
    const defaultTarget = exported.find(each => each.name === 'default')?.target;
    // The instance receivers listed, for the scope pass (carrick#2774).
    const instances: ScopeSource[] = [];
    for (const { name, target, type } of exported) {
      if (!take()) continue;
      const entry: SurfaceExport = { export: name, receivers: [] };
      surface.exports.push(entry);
      if (this.isOpenTop(type)) continue;
      const receiverOf = (receiverName: string, receiverType: ts.Type): void => {
        if (!take()) return;
        const home = this.homeOf(pkg, [target, ...typeSymbols(receiverType)]);
        this.typeContext = name;
        if (receiverName.startsWith('instance:')) this.noteType(receiverType);
        const receiver = this.outlineReceiver(pkg, receiverName, receiverType, home, take, names, handlers, fills);
        entry.receivers.push(receiver);
        if (receiverName.startsWith('instance:')) {
          instances.push({ export: name, target, type: receiverType, home, receiver });
        }
      };
      receiverOf('export', type);
      for (const maker of this.makers(pkg, target, type)) {
        const made = this.madeBy(maker.signatures, built(name, maker.receiver), target === defaultTarget);
        if (!made) continue;
        // One level below the export, what the member builds must declare a callable member.
        if (
          maker.member &&
          !this.namedProperties(made).some(
            member => this.librarySignatures(this.checker.getTypeOfSymbol(member), 'call').length > 0
          )
        ) {
          continue;
        }
        receiverOf(maker.receiver, made);
      }
    }
    for (const run of names) run();
    if (withHandlers) for (const run of handlers) run();
    for (const fill of fills) fill();
    this.listScopeReceivers(pkg, instances, maxEntries, withHandlers, built);
    surface.truncated = dropped;
    this.collectTypes = false;
    if (withHandlers) surface.handler_types = this.handlerTypes;
    if (withHandlers || this.listTypes) surface.types = this.listTypeReceivers(pkg, typeExports, maxEntries);
    if (withHandlers) surface.handler_params_left_out = this.handlerParamsLeftOut;
    if (this.listTypes) surface.types_left_out = this.typesLeftOut;
    return surface;
  }

  /**
   * The sixth pass (carrick#2977): each type export a `handler_types` entry
   * names, sorted by name, as the one receiver `type`, built at its declared
   * type-parameter defaults as the verifier builds it (`typeBuildLine`), and
   * outlined as any receiver is, without handlers. Everything it lists draws
   * on one budget of its own, so `truncated` and the hash are unchanged.
   */
  private listTypeReceivers(pkg: string, typeExports: readonly BuiltType[], maxEntries: number): SurfaceTypeExport[] {
    if (this.listTypes) return this.listWidenedTypes(pkg, typeExports, maxEntries);
    const named = [...new Set(this.handlerTypes.flatMap(entry => (entry.export === undefined ? [] : [entry.export])))].sort(
      byName
    );
    let budget = maxEntries;
    const take = (): boolean => budget > 0 && (budget--, true);
    const names: Array<() => void> = [];
    const fills: Array<() => void> = [];
    const listed: SurfaceTypeExport[] = [];
    for (const name of named) {
      const built = typeExports.find(each => each.name === name);
      if (built === undefined || !take()) continue;
      const home = this.homeOf(pkg, [built.symbol, ...typeSymbols(built.type)]);
      listed.push({
        export: name,
        receivers: [this.outlineReceiver(pkg, TYPE_RECEIVER, built.type, home, take, names, [], fills)],
      });
    }
    for (const run of names) run();
    for (const fill of fills) fill();
    return listed;
  }

  /**
   * `types[]` under `types: true` (carrick#3050): every exported class, and
   * every type the surface's makers, members and handlers reach that has a
   * receiver id (`idOfSymbol`), sorted by id, each outlined as any receiver
   * is, with the properties a service may assign. A `type` id reads the type
   * the probe built at its defaults; any other, the type as reached when
   * that is its defaults (`atDefaults`). One budget of its own: an entry it
   * leaves out is counted in `types_left_out`.
   */
  private listWidenedTypes(pkg: string, typeExports: readonly BuiltType[], maxEntries: number): SurfaceTypeExport[] {
    const builtBySymbol = new Map(typeExports.map(each => [each.symbol, each] as const).reverse());
    const entries = new Map<ts.Symbol, TypeCandidate>();
    for (const built of typeExports) {
      if ((built.symbol.flags & ts.SymbolFlags.Class) === 0 || entries.has(built.symbol)) continue;
      entries.set(built.symbol, { id: TYPE_RECEIVER, export: built.name, symbol: built.symbol, type: built.type });
    }
    for (const candidate of this.typeCandidates.values()) {
      if (entries.has(candidate.symbol)) continue;
      if (candidate.id !== TYPE_RECEIVER) {
        entries.set(candidate.symbol, candidate);
        continue;
      }
      const built = builtBySymbol.get(candidate.symbol);
      if (built !== undefined) entries.set(candidate.symbol, { ...candidate, type: built.type });
    }
    const ordered = [...entries.values()].sort((a, b) => byName(a.id, b.id) || byName(a.export, b.export));
    let budget = maxEntries;
    const take = (): boolean => budget > 0 && (budget--, true);
    const names: Array<() => void> = [];
    const fills: Array<() => void> = [];
    const listed: SurfaceTypeExport[] = [];
    for (const entry of ordered) {
      if (!take()) {
        this.typesLeftOut += 1;
        continue;
      }
      const home = this.homeOf(pkg, [entry.symbol, ...typeSymbols(entry.type)]);
      const receiver = this.outlineReceiver(pkg, entry.id, entry.type, home, take, names, [], fills);
      const settable = this.settableOf(pkg, entry.type, home);
      if (settable.length > 0) receiver.settable = settable;
      listed.push({ export: entry.export, receivers: [receiver] });
    }
    for (const run of names) run();
    for (const fill of fills) fill();
    return listed;
  }

  /** The properties of a type a service may assign, depth 1, by name (`settable`). */
  private settableOf(pkg: string, type: ts.Type, home: ReadonlySet<string>): SurfaceSettable[] {
    const holder = this.surfaceHolder(pkg, type, home);
    const listed: SurfaceSettable[] = [];
    for (const property of sortedByName(this.namedProperties(type))) {
      if (this.settable(holder, property) !== 'ok') continue;
      listed.push({
        name: property.getName(),
        type: this.printed(this.checker.getTypeOfSymbol(property)),
        own: !('failure' in this.ownMember(holder, property.getName())),
      });
    }
    return listed;
  }

  /**
   * Whether a service may assign `property` of `holder`, as a reply body is
   * set (`ctx.body = x`): a property, not a method or a function-typed one
   * (`set_not_property`); not readonly and not get-only (`set_readonly`); its
   * declared type open, `unknown` or an unconstrained type parameter
   * (`set_not_open`), which is what tells a body from a setter such as a
   * status. No member name is read.
   */
  settable(holder: Holder, property: ts.Symbol): 'ok' | 'set_not_property' | 'set_readonly' | 'set_not_open' {
    const read = this.declaredProperty(holder.type, property.getName()) ?? property;
    const type = this.checker.getTypeOfSymbol(read);
    if (
      (read.flags & ts.SymbolFlags.Method) !== 0 ||
      this.parts(type).some(part => part.getCallSignatures().length > 0 || part.getConstructSignatures().length > 0)
    ) {
      return 'set_not_property';
    }
    const declarations = read.declarations ?? [];
    const getOnly = (read.flags & ts.SymbolFlags.GetAccessor) !== 0 && (read.flags & ts.SymbolFlags.SetAccessor) === 0;
    if (
      getOnly ||
      (declarations.length > 0 &&
        declarations.every(declaration => (ts.getCombinedModifierFlags(declaration) & ts.ModifierFlags.Readonly) !== 0))
    ) {
      return 'set_readonly';
    }
    return this.isOpenBody(type) ? 'ok' : 'set_not_open';
  }

  /**
   * Note a type for `types[]` when it has a receiver id (carrick#3050), under
   * the export being read. A type seen at other than its type-parameter
   * defaults is not noted, unless its id says the probe builds it (`type`).
   */
  private noteType(type: ts.Type): void {
    if (!this.listTypes || type.isUnion() || (type.flags & ts.TypeFlags.TypeParameter) !== 0) return;
    const symbol = type.aliasSymbol ?? type.getSymbol();
    if (symbol === undefined || this.typeCandidates.has(symbol)) return;
    const id = this.idOfSymbol(symbol);
    if (id === undefined || (id.id !== TYPE_RECEIVER && !this.atDefaults(type, symbol))) return;
    this.typeCandidates.set(symbol, { id: id.id, export: id.export ?? this.typeContext, symbol, type });
  }

  /** A type a signature returns, awaited and without null, noted for `types[]`. */
  private noteReturn(signature: ts.Signature): void {
    const returned = this.checker.getNonNullableType(this.checker.getReturnTypeOfSignature(signature));
    this.noteType(this.checker.getNonNullableType(this.checker.getAwaitedType(returned) ?? returned));
  }

  /** `type` is `symbol` read at its declared type-parameter defaults: it has none, or its arguments are exactly the defaults. */
  private atDefaults(type: ts.Type, symbol: ts.Symbol): boolean {
    const declaration = (symbol.declarations ?? []).find(
      (each): each is ts.InterfaceDeclaration | ts.TypeAliasDeclaration | ts.ClassDeclaration =>
        (ts.isInterfaceDeclaration(each) || ts.isTypeAliasDeclaration(each) || ts.isClassDeclaration(each)) &&
        (each.typeParameters?.length ?? 0) > 0
    );
    if (declaration === undefined) return true;
    const args = type.aliasSymbol === symbol
      ? type.aliasTypeArguments ?? []
      : ((type as ts.ObjectType).objectFlags ?? 0) & ts.ObjectFlags.Reference
        ? this.checker.getTypeArguments(type as ts.TypeReference)
        : [];
    return declaration.typeParameters!.every((node, index) => {
      const parameter = this.checker.getTypeAtLocation(node);
      const fallback = this.checker.getDefaultFromTypeParameter(parameter);
      return fallback !== undefined && fallback === args[index];
    });
  }

  /**
   * The receiver id of a class, interface or alias (amendment 7), or
   * undefined when it has none. Declared in the specifier's own packages:
   * `type` when the specifier exports it (the export), `type:<Path>` when an
   * export's namespace declares it (that export). Declared in another
   * installed package: `type:<module>#<Path>`, the module as a service
   * imports it (`@types/x` is `x`; an ambient `declare module 'm'` is `m`),
   * whichever specifier re-exports it. Anonymous types, the default
   * library's and the runtime's types have none.
   */
  private idOfSymbol(symbol: ts.Symbol): TypeId | undefined {
    const known = this.typeIds.get(symbol);
    if (known !== undefined) return known ?? undefined;
    const found = this.readIdOfSymbol(symbol);
    this.typeIds.set(symbol, found ?? null);
    return found;
  }

  private readIdOfSymbol(symbol: ts.Symbol): TypeId | undefined {
    if ((symbol.flags & (ts.SymbolFlags.Class | ts.SymbolFlags.Interface | ts.SymbolFlags.TypeAlias)) === 0) return undefined;
    const declarations = symbol.declarations ?? [];
    if (declarations.length === 0 || !IDENTIFIER.test(symbol.getName())) return undefined;
    if (declarations.some(declaration => this.program.isSourceFileDefaultLibrary(declaration.getSourceFile()))) return undefined;
    const first = declarations[0];
    // The namespaces around the declaration, outermost first.
    const containers: ts.ModuleDeclaration[] = [];
    for (let node: ts.Node | undefined = first.parent; node !== undefined; node = node.parent) {
      if (ts.isModuleDeclaration(node)) containers.unshift(node);
    }
    if (declarations.some(declaration => this.isOwnFile(declaration.getSourceFile(), this.specifierHome))) {
      const exported = this.typeExportNames.get(symbol);
      if (exported !== undefined) return { id: TYPE_RECEIVER, export: exported };
      const outer = containers[0] === undefined ? undefined : this.checker.getSymbolAtLocation(containers[0].name);
      const owner = outer === undefined ? undefined : this.exportTargets.get(outer);
      if (owner === undefined || !containers.every(each => ts.isIdentifier(each.name))) return undefined;
      const names = [...containers.slice(1).map(each => (each.name as ts.Identifier).text), symbol.getName()];
      return { id: `type:${names.join('.')}`, export: owner };
    }
    const owner = this.packageOf(first.getSourceFile());
    if (owner === undefined || RUNTIME_TYPE_PACKAGES.has(owner)) return undefined;
    let module = publicNameOf(owner);
    let names = containers.filter(each => ts.isIdentifier(each.name)).map(each => (each.name as ts.Identifier).text);
    const ambient = containers.findIndex(each => ts.isStringLiteral(each.name));
    if (ambient >= 0) {
      module = (containers[ambient].name as ts.StringLiteral).text;
      names = containers.slice(ambient + 1).map(each => (each.name as ts.Identifier).text);
    } else if (names.length > 0 && this.isExportEqualsTarget(first.getSourceFile(), names[0])) {
      // The namespace a module exports whole is the module itself.
      names = names.slice(1);
    }
    return { id: `type:${module}#${[...names, symbol.getName()].join('.')}` };
  }

  /** The file says `export = name`. */
  private isExportEqualsTarget(file: ts.SourceFile, name: string): boolean {
    return file.statements.some(
      statement =>
        ts.isExportAssignment(statement) &&
        statement.isExportEquals === true &&
        ts.isIdentifier(statement.expression) &&
        statement.expression.text === name
    );
  }

  /**
   * The fifth pass (carrick#2774): each own member of an instance receiver
   * whose call returns the object routes are registered on lists it as
   * `scope_receiver`, outlined as any receiver is. Everything it lists draws
   * on one budget of its own, spent after every other pass, so the listing
   * without it, `truncated` and the hash are the ones the lister gave before.
   * One level: a scope receiver lists no scope receiver of its own.
   */
  private listScopeReceivers(
    pkg: string,
    sources: readonly ScopeSource[],
    maxEntries: number,
    withHandlers: boolean,
    built: (exportName: string, receiver: string) => ts.Signature | undefined
  ): void {
    let budget = maxEntries;
    const take = (): boolean => budget > 0 && (budget--, true);
    const names: Array<() => void> = [];
    const handlers: Array<() => void> = [];
    const fills: Array<() => void> = [];
    for (const source of sources) {
      const holder = this.surfaceHolder(pkg, source.type, source.home);
      for (const member of source.receiver.members) {
        const id = member.own ? scopeReceiverId(source.receiver.receiver, member.name) : undefined;
        if (id === undefined) continue;
        const callee = this.ownCallee(holder, [], member.name, 'call');
        if (!('signatures' in callee)) continue;
        if (!callee.signatures.some(signature => this.takesString(signature))) continue;
        const type = this.scopeTypeOf(callee.signatures, built(source.export, id), source.type, source.home);
        if (type === undefined || !take()) continue;
        const home = this.homeOf(pkg, [source.target, ...typeSymbols(type)]);
        this.typeContext = source.export;
        this.noteType(type);
        member.scope_receiver = this.outlineReceiver(pkg, id, type, home, take, names, handlers, fills);
      }
    }
    for (const run of names) run();
    if (withHandlers) for (const run of handlers) run();
    for (const fill of fills) fill();
  }

  /**
   * The receivers each export of a specifier can make, by receiver id, for
   * the probe statements that build them (see `LibraryClaimsVerifier.listSurface`).
   */
  makersOf(
    pkg: string,
    probe: SurfaceProbe,
    only: readonly string[] | undefined
  ): { makers: { export: string; receivers: string[]; scopes: string[] }[]; types: string[]; classes: string[] } {
    this.setPackage(pkg);
    const moduleRead = 'namespace' in probe ? this.readModule(pkg, probe.namespace, true) : undefined;
    if (moduleRead?.reason) return { makers: [], types: [], classes: [] };
    const readable = 'namespace' in probe && !moduleRead?.globalFile;
    const types = readable ? this.typeExportCandidates(probe.namespace, false) : [];
    const classes = readable ? this.typeExportCandidates(probe.namespace, true) : [];
    const makers = (this.listedExports(probe, only, moduleRead?.globalFile) ?? [])
      .filter(({ type }) => !this.isOpenTop(type))
      .map(({ name, target, type }) => {
        const makers = this.makers(pkg, target, type);
        return {
          export: name,
          receivers: makers.map(maker => maker.receiver),
          scopes: this.scopeCandidates(pkg, target, makers),
        };
      })
      .filter(({ receivers }) => receivers.length > 0);
    return { makers, types, classes };
  }

  /**
   * The type-only exports of a specifier a handler's parameter may be
   * (carrick#2977 decision 7), sorted by name: an interface or a type alias
   * with no value meaning, every type parameter defaulted, so the probe can
   * build it with no type argument. Every one, whatever `only` names: the
   * value exports a caller wants hand these types to their handlers. A
   * superset: the listing names one only when a parameter is that
   * declaration and it is declared in the handing receiver's home.
   */
  private typeExportCandidates(namespace: ts.ImportDeclaration, classes: boolean): string[] {
    const moduleSymbol = this.checker.getSymbolAtLocation(namespace.moduleSpecifier);
    if (!moduleSymbol) return [];
    const names: string[] = [];
    for (const symbol of this.checker.getExportsOfModule(moduleSymbol)) {
      const target = symbol.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(symbol) : symbol;
      const wanted = classes
        ? (target.flags & ts.SymbolFlags.Class) !== 0 && symbol.getName() !== 'default'
        : (target.flags & ts.SymbolFlags.Value) === 0 &&
          (target.flags & (ts.SymbolFlags.Interface | ts.SymbolFlags.TypeAlias)) !== 0;
      if (IDENTIFIER.test(symbol.getName()) && !this.checker.isUnknownSymbol(target) && wanted && typeParametersDefaulted(target)) {
        names.push(symbol.getName());
      }
    }
    return names.sort(byName);
  }

  /**
   * The `R>scope:m` receivers an export's `instance:` makers may have
   * (carrick#2774): for each object type a maker's overloads declare as their
   * return, each own member that `scopeTypeOf` lists at its declared return.
   * A superset of the listing, which reads each at the type the probe's build
   * resolves; the second program builds these so `listPackage` can.
   */
  private scopeCandidates(pkg: string, target: ts.Symbol, makers: readonly Maker[]): string[] {
    const found = new Set<string>();
    for (const maker of makers) {
      if (!maker.receiver.startsWith('instance:')) continue;
      const types = new Set(maker.signatures.map(sig => this.checker.getReturnTypeOfSignature(sig)));
      for (const type of types) {
        if (this.returnSaysNothing(type) || !this.isObjectType(type)) continue;
        const home = this.homeOf(pkg, [target, ...typeSymbols(type)]);
        for (const name of this.scopeMembers(pkg, type, home)) {
          const id = scopeReceiverId(maker.receiver, name);
          if (id !== undefined) found.add(id);
        }
      }
    }
    return [...found];
  }

  /** The members of `type` that list a scope receiver at their declared return, read once per type and home. */
  private scopeMembers(pkg: string, type: ts.Type, home: ReadonlySet<string>): readonly string[] {
    const key = [...home].sort().join('\u0000');
    let byHome = this.scopeMembersRead.get(type);
    if (byHome === undefined) this.scopeMembersRead.set(type, (byHome = new Map()));
    const read = byHome.get(key);
    if (read !== undefined) return read;
    const names: string[] = [];
    const holder = this.surfaceHolder(pkg, type, home);
    for (const property of sortedByName(this.namedProperties(type))) {
      if (!this.mayScope(property)) continue;
      const callee = this.ownCallee(holder, [], property.getName(), 'call');
      if ('signatures' in callee && this.scopeTypeOf(callee.signatures, undefined, type, home) !== undefined) {
        names.push(property.getName());
      }
    }
    byHome.set(key, names);
    return names;
  }

  /** Some library signature of the member takes a string: the cheap superset of what `scopeTypeOf` asks first. */
  private mayScope(property: ts.Symbol): boolean {
    return this.librarySignatures(this.checker.getTypeOfSymbol(property), 'call').some(signature =>
      this.takesString(signature)
    );
  }

  /**
   * The object a scope member returns, as the verifier reads a scope step
   * (carrick#2774): what a route is registered on when the member is called
   * with a path. Undefined unless:
   * - some own signature takes a string, positionally or at a key of an
   *   object parameter;
   * - the returns (at `built`'s defaults, less those that say nothing) are
   *   all object types, none thenable, sharing one declared symbol that is
   *   declared in the receiver's home and has a callable member of its own;
   * - that symbol and the receiver's share no declaration in the home
   *   (`family`): the receiver itself (`basePath(): this`), a sibling or a
   *   wrapper of one base is a builder's next link, not an object routes
   *   are registered on.
   * The type is the return of the overload `built` resolves, when that is
   * one of the signatures that take a string, else the first of them.
   */
  private scopeTypeOf(
    signatures: readonly ts.Signature[],
    built: ts.Signature | undefined,
    receiver: ts.Type,
    home: ReadonlySet<string>
  ): ts.Type | undefined {
    const takers = signatures.filter(signature => this.takesString(signature));
    const returns = takers
      .map(signature => ({ signature, type: this.returnOf(signature, built) }))
      .filter(({ type }) => !this.returnSaysNothing(type));
    if (returns.length === 0) return undefined;
    const symbolOf = (type: ts.Type): ts.Symbol | undefined => type.getSymbol() ?? type.aliasSymbol;
    const symbol = symbolOf(returns[0].type);
    if (symbol === undefined) return undefined;
    // The receiver itself, a sibling of it, or a wrapper built from the same
    // base (a fluent builder's `.default(x)`) is the same kind of thing, not
    // an object routes are registered on: abstain.
    const mine = this.family(receiver, home);
    if ([...this.family(returns[0].type, home)].some(each => mine.has(each))) return undefined;
    for (const { type } of returns) {
      if (!this.isObjectType(type) || symbolOf(type) !== symbol || this.isThenable(type)) return undefined;
    }
    if (!(symbol.declarations ?? []).some(declaration => this.isOwnFile(declaration.getSourceFile(), home))) return undefined;
    const chosen = returns.find(({ signature }) => built !== undefined && this.instantiates(built, signature)) ?? returns[0];
    const callable = this.namedProperties(chosen.type).some(
      member => this.librarySignatures(this.checker.getTypeOfSymbol(member), 'call').length > 0
    );
    return callable ? chosen.type : undefined;
  }

  /** The type's own declaration and every base it extends or implements that its home declares. */
  private family(type: ts.Type, home: ReadonlySet<string>): Set<ts.Symbol> {
    const found = new Set<ts.Symbol>();
    const visit = (each: ts.Type, depth: number): void => {
      const target = ((each as ts.ObjectType).objectFlags ?? 0) & ts.ObjectFlags.Reference ? (each as ts.TypeReference).target : each;
      const symbol = target.getSymbol();
      if (symbol === undefined || found.has(symbol) || depth > 12) return;
      if (!(symbol.declarations ?? []).some(declaration => this.isOwnFile(declaration.getSourceFile(), home))) return;
      found.add(symbol);
      if (((target as ts.ObjectType).objectFlags ?? 0) & ts.ObjectFlags.ClassOrInterface) {
        for (const base of this.checker.getBaseTypes(target as ts.InterfaceType)) visit(base, depth + 1);
      }
    };
    visit(type, 0);
    return found;
  }

  /** A parameter of the signature accepts a string, positionally or at a key of its object. */
  private takesString(signature: ts.Signature): boolean {
    let read = this.takesStringRead.get(signature);
    if (read === undefined) this.takesStringRead.set(signature, (read = this.readTakesString(signature)));
    return read;
  }

  private readTakesString(signature: ts.Signature): boolean {
    const count = signature.getParameters().length;
    for (let index = 0; index < count; index++) {
      const slot = this.keyParameterAt(signature, index);
      if (slot === undefined || slot === VARIADIC) continue;
      if (this.acceptsString(this.throughConditional(slot))) return true;
      if (this.isObjectType(slot) && this.handlerFailure(slot) !== undefined) {
        if (this.listKeys(slot, () => true).some(key => key.accepts_string)) return true;
      }
    }
    return false;
  }

  /** The type has a callable `then`: awaiting it gives something else. */
  private isThenable(type: ts.Type): boolean {
    const then = this.checker.getPropertyOfType(type, 'then');
    return then !== undefined && this.checker.getNonNullableType(this.checker.getTypeOfSymbol(then)).getCallSignatures().length > 0;
  }

  /**
   * The value exports a specifier lists (every one, or only those `only`
   * names), sorted by name: what its namespace holds, and `default` for a
   * module that exports a value whole (`export =`), as a default import gets
   * it, when that import reads a typed value (`readExport`, as the verifier
   * reads `default`). Such a module's namespace holds its statics, and its
   * `prototype`, which no service imports. Undefined when the specifier
   * resolves to no module symbol.
   */
  private listedExports(
    probe: SurfaceProbe,
    only: readonly string[] | undefined,
    globalFile?: ts.SourceFile
  ): ListedExport[] | undefined {
    if ('defaultLibrary' in probe) return this.listedGlobals(this.defaultLibraryFile(probe.defaultLibrary), only);
    if (globalFile) return this.listedGlobals(globalFile, only);
    const moduleSymbol = this.checker.getSymbolAtLocation(probe.namespace.moduleSpecifier);
    if (!moduleSymbol) return undefined;
    const wanted = (name: string) => only === undefined || only.includes(name);
    const all = this.checker.getExportsOfModule(moduleSymbol);
    const listed: ListedExport[] = [];
    for (const symbol of all) {
      const target = symbol.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(symbol) : symbol;
      if (
        this.checker.isUnknownSymbol(target) ||
        (target.flags & ts.SymbolFlags.Value) === 0 ||
        (target.flags & ts.SymbolFlags.Prototype) !== 0 ||
        !wanted(symbol.getName())
      ) {
        continue;
      }
      listed.push({ name: symbol.getName(), target, type: this.checker.getTypeOfSymbolAtLocation(symbol, probe.namespace) });
    }
    if (
      moduleSymbol.exports?.has(ts.InternalSymbolName.ExportEquals) &&
      !all.some(symbol => symbol.getName() === 'default') &&
      wanted('default')
    ) {
      const read = this.readExport(probe.defaultImport);
      if (!('reason' in read)) listed.push({ name: 'default', target: read.value.target, type: read.value.type });
    }
    return listed.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  }

  /**
   * The value globals a `lib:` key's file declares (every one, or only those
   * `only` names), sorted by name, each with its declared type: none when the
   * program's `lib` setting does not load the file.
   */
  private listedGlobals(file: ts.SourceFile | undefined, only: readonly string[] | undefined): ListedExport[] {
    if (!file) return [];
    return this.checker
      .getSymbolsInScope(this.probe, ts.SymbolFlags.Value)
      .filter(
        symbol =>
          (symbol.flags & ts.SymbolFlags.Prototype) === 0 &&
          (only === undefined || only.includes(symbol.getName())) &&
          (symbol.declarations ?? []).some(declaration => declaration.getSourceFile() === file)
      )
      .map(symbol => ({ name: symbol.getName(), target: symbol, type: this.checker.getTypeOfSymbol(symbol) }))
      .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
  }

  /**
   * The ways an export makes a receiver, in listing order: calling it
   * (`instance:()`), constructing it (`instance:new`), then calling or
   * constructing each member (`instance:<member>`, `instance:new:<member>`),
   * each through the signatures a maker claim reads there (`ownCallee`): its
   * home's own, or a base's it binds with its own type. A maker the export
   * only inherits from another package is none the verifier reads
   * (`member_inherited`).
   */
  private makers(pkg: string, target: ts.Symbol, type: ts.Type): Maker[] {
    const holder = this.surfaceHolder(pkg, type, this.homeOf(pkg, [target, ...typeSymbols(type)]));
    const makers: Maker[] = [];
    for (const [form, receiver] of [['call', 'instance:()'], ['new', 'instance:new']] as const) {
      const callee = this.ownCallee(holder, [], null, form);
      if ('signatures' in callee) makers.push({ receiver, member: false, signatures: callee.signatures });
    }
    for (const property of sortedByName(this.namedProperties(type))) {
      for (const [form, prefix] of [['call', 'instance:'], ['new', 'instance:new:']] as const) {
        const callee = this.ownCallee(holder, [], property.getName(), form);
        if ('signatures' in callee) {
          makers.push({ receiver: `${prefix}${property.getName()}`, member: true, signatures: callee.signatures });
        }
      }
    }
    return makers;
  }

  /** A receiver read for the listing as a message claim reads it (`ownCallee`). */
  private surfaceHolder(pkg: string, type: ts.Type, home: ReadonlySet<string>): MessageReceiver {
    return { type, pkg, home, makerBindsName: false, scoped: false };
  }

  /**
   * What a maker builds: each overload's return, read at its declared
   * type-parameter defaults where `built` (the probe's no-argument build of
   * the receiver) instantiates it (`returnOf`), less the returns that say
   * nothing, which no maker claim holds on. Listed when what is left is one
   * object type. A `default` export, which a service imports under any local
   * name (usually the one maker a framework has), or the same declaration
   * exported by name (carrick#2640), that is overloaded and so
   * returns several types is listed for the return of the overload the
   * probe's no-argument build resolves (`built`), when that is an object type
   * with members of its own.
   */
  private madeBy(
    signatures: readonly ts.Signature[],
    built: ts.Signature | undefined,
    isDefault: boolean
  ): ts.Type | undefined {
    const returns = [
      ...new Set(signatures.map(sig => this.returnOf(sig, built)).filter(type => !this.returnSaysNothing(type))),
    ];
    if (returns.length === 1) return this.isObjectType(returns[0]) ? returns[0] : undefined;
    if (!isDefault || returns.length === 0 || built === undefined) return undefined;
    const resolved = this.checker.getReturnTypeOfSignature(built);
    return !this.returnSaysNothing(resolved) && this.isObjectType(resolved) && this.namedProperties(resolved).length > 0
      ? resolved
      : undefined;
  }

  /** The call (or construct) signatures of `type` an installed package or the default library declares. */
  private librarySignatures(type: ts.Type, form: 'call' | 'new'): readonly ts.Signature[] {
    const nonNullable = this.checker.getNonNullableType(type);
    const all = form === 'new' ? nonNullable.getConstructSignatures() : nonNullable.getCallSignatures();
    return all.filter(signature => {
      const file = this.signatureFile(signature, nonNullable, form);
      return file !== undefined && this.isLibraryFile(file);
    });
  }

  /**
   * A receiver, now; its member names later (`names`), once every export and
   * receiver of the package is listed; the members of its handlers'
   * parameters next (`handlers`), once every name is; its call, construct and
   * member signatures last (`fills`), once every handler is.
   */
  private outlineReceiver(
    pkg: string,
    receiverName: string,
    type: ts.Type,
    home: ReadonlySet<string>,
    take: () => boolean,
    names: Array<() => void>,
    handlers: Array<() => void>,
    fills: Array<() => void>
  ): SurfaceReceiver {
    const receiver: SurfaceReceiver = { receiver: receiverName, members: [] };
    const holder = this.surfaceHolder(pkg, type, home);
    const context = this.typeContext;
    // The receiver's own call and construct signatures, as a claim with a
    // null member reads them.
    const call = this.ownCallee(holder, [], null, 'call');
    if ('signatures' in call) fills.push(() => (receiver.call = this.listSignatures(call.signatures, holder.type, take)));
    const construct = this.ownCallee(holder, [], null, 'new');
    if ('signatures' in construct) {
      fills.push(() => (receiver.construct = this.listSignatures(construct.signatures, holder.type, take)));
    }
    if (this.isOpenTop(type)) return receiver;
    names.push(() => {
      for (const property of sortedByName(this.namedProperties(type))) {
        const library = this.librarySignatures(this.checker.getTypeOfSymbol(property), 'call');
        if (library.length === 0 || !take()) continue;
        // Own when an op claim reads the member at all (`ownCallee`): its
        // home declares it, or it sits on a base the receiver binds with its
        // own type, and its home writes a signature of it. An inherited
        // member is listed with every library signature.
        const callee = this.ownCallee(holder, [], property.getName(), 'call');
        const own = 'signatures' in callee;
        const member: SurfaceMember = { name: property.getName(), own, signatures: [] };
        receiver.members.push(member);
        if (this.collectTypes) {
          this.typeContext = context;
          for (const signature of own ? callee.signatures : library) this.noteReturn(signature);
        }
        handlers.push(() => {
          this.typeContext = context;
          const listed = this.listHandlers(own ? callee.signatures : library, home);
          if (listed.length > 0) member.handlers = listed;
        });
        fills.push(() => {
          member.signatures = this.listSignatures(own ? callee.signatures : library, holder.type, take);
          // The runtime's own emitter members take the names the runtime
          // gives every emitter, besides the ones a signature spells.
          if (!RUNTIME_MODULE.test(pkg) || !this.isRuntimeEmitterProperty(property)) return;
          const spelled = new Set(member.signatures.flatMap(s => s.params.flatMap(p => p.literals ?? [])));
          const events = RUNTIME_EMITTER_EVENTS.filter(name => !spelled.has(name));
          if (events.length > 0) member.runtime_events = events;
        });
      }
    });
    return receiver;
  }

  /**
   * The functions a member's signatures take, at a positional argument or at a
   * key of an options object, with the members their parameters declare. A
   * handler is listed once per place, from the first signature that states it.
   * A parameter that declares no member lists nothing, and so does a handler
   * none of whose parameters do.
   */
  private listHandlers(signatures: readonly ts.Signature[], home: ReadonlySet<string>): SurfaceHandler[] {
    const listed: SurfaceHandler[] = [];
    const seen = new Set<string>();
    const add = (arg: number, key: string | undefined, slot: ts.Type): void => {
      const place = JSON.stringify([arg, key ?? null]);
      if (seen.has(place)) return;
      const params = this.listHandlerParams(slot, home);
      if (params.length === 0) return;
      seen.add(place);
      listed.push(key === undefined ? { arg, params } : { arg, key, params });
    };
    for (const signature of signatures) {
      const count = signature.getParameters().length;
      for (let arg = 0; arg < count; arg++) {
        const slot = this.keyParameterAt(signature, arg);
        if (slot === undefined || slot === VARIADIC) continue;
        if (this.handlerFailure(slot) === undefined) {
          add(arg, undefined, slot);
          continue;
        }
        if (!this.isObjectType(slot)) continue;
        const keys = new Map<string, ts.Type>();
        for (const view of this.keyViews(slot)) {
          for (const property of this.namedProperties(view)) {
            const type = this.checker.getTypeOfSymbol(property);
            if (!keys.has(property.getName()) && this.handlerFailure(type) === undefined) keys.set(property.getName(), type);
          }
        }
        for (const key of [...keys.keys()].sort(byName)) add(arg, key, keys.get(key)!);
      }
    }
    return listed;
  }

  /**
   * The parameters of a function type's first signature that declare
   * members, each by the id of its type's entry (`handlerTypeId`). A
   * parameter whose new entry would pass the listing's budget is left out
   * and counted (carrick#3016).
   */
  private listHandlerParams(handler: ts.Type, home: ReadonlySet<string>): SurfaceHandlerParam[] {
    const slot = this.throughConditional(this.throughConstraint(handler));
    if (slot === VARIADIC) return [];
    const signature = this.parts(slot)
      .flatMap(part => part.getCallSignatures())
      .find(each => each.getParameters().length > 0);
    if (!signature) return [];
    const listed: SurfaceHandlerParam[] = [];
    signature.getParameters().forEach((parameter, index) => {
      const type = this.checker.getTypeOfSymbol(parameter);
      if (this.namedProperties(type).length === 0) return;
      const id = this.handlerTypeId(type, home);
      if (id === 0) {
        this.handlerParamsLeftOut += 1;
        return;
      }
      listed.push({ param: index, name: parameter.getName(), type_id: id });
    });
    return listed;
  }

  /**
   * The id of a handler parameter's entry in the surface's `handler_types`
   * (carrick#2977), 0 when the listing's budget leaves it out. One lookup per
   * parameter by type object and export name; a type object seen for the
   * first time is keyed by content (its printed type, members and export
   * name), so equal contents share one entry and a resolved reference is
   * byte for byte the inline parameter the lister wrote before. Only a new
   * entry spends the budget: its members are written once.
   */
  private handlerTypeId(type: ts.Type, home: ReadonlySet<string>): number {
    let exportName = this.nameableExport(type, home);
    if (this.listTypes) {
      // A handed type declared in another package carries its receiver id as
      // its `export` (carrick#3050); the ones declared in the home are as
      // they were.
      this.noteType(type);
      const symbol = type.aliasSymbol ?? type.getSymbol();
      const id = symbol === undefined || type.isUnion() ? undefined : this.idOfSymbol(symbol);
      if (id !== undefined && id.id.includes('#')) exportName = id.id;
    }
    let byExport = this.handlerTypeIds.get(type);
    if (byExport === undefined) this.handlerTypeIds.set(type, (byExport = new Map()));
    const known = byExport.get(exportName);
    if (known !== undefined) return known;
    const { members, dropped } = this.handlerMembersOf(type);
    const printed = this.printed(type);
    const content = JSON.stringify([printed, members, exportName]);
    let id = this.handlerTypeByContent.get(content);
    if (id === undefined) {
      if (members.length > this.handlerBudget) {
        id = 0;
      } else {
        this.handlerBudget -= members.length;
        id = this.handlerTypes.length + 1;
        const entry: SurfaceHandlerType = { id, type: printed, members, dropped };
        if (exportName !== null) entry.export = exportName;
        this.handlerTypes.push(entry);
      }
      this.handlerTypeByContent.set(content, id);
    }
    byExport.set(exportName, id);
    return id;
  }

  /**
   * The type export of the surface being listed that a handler parameter's
   * type is (carrick#2977 decision 7): the same symbol (its alias's, then its
   * own), never the same name, with a declaration in `home`, the packages of
   * the receiver whose member hands it, as `yieldsOutcome` reads a `yields`.
   */
  private nameableExport(type: ts.Type, home: ReadonlySet<string>): string | null {
    if (this.typeExportNames.size === 0) return null;
    for (const symbol of [type.aliasSymbol, type.getSymbol()]) {
      const name = symbol === undefined ? undefined : this.typeExportNames.get(symbol);
      if (
        name !== undefined &&
        (symbol!.declarations ?? []).some(declaration => this.isOwnFile(declaration.getSourceFile(), home))
      ) {
        return name;
      }
    }
    return null;
  }

  /**
   * A handler parameter's declared members by path, down to
   * `HANDLER_MEMBER_DEPTH`, at most `HANDLER_MEMBER_CAP` of them (carrick#2966).
   * The cap keeps every top-level member before any nested one, so a wide
   * parameter loses its deepest members, never a top-level one a later member
   * name sorts after. Kept members come back parents before children, each
   * level sorted by name. Read once per type in a package, across its
   * specifiers: a type many handlers share is listed once.
   */
  private handlerMembersOf(type: ts.Type): { members: SurfaceHandlerMember[]; dropped: number } {
    const known = this.handlerMembers.get(type);
    if (known) return known;
    surfaceReadCounts.handlerMemberBuilds += 1;
    interface Node { member: SurfaceHandlerMember; children: Node[] }
    const roots: Node[] = [];
    let kept = 0;
    let dropped = 0;
    // Breadth first: a level is kept whole, in name order, before the next.
    let level: Array<{ type: ts.Type; prefix: string[]; into: Node[] }> = [{ type, prefix: [], into: roots }];
    while (level.length > 0) {
      const next: typeof level = [];
      for (const { type: holder, prefix, into } of level) {
        for (const property of sortedByName(this.namedProperties(holder))) {
          if (kept >= HANDLER_MEMBER_CAP) {
            dropped += 1;
            continue;
          }
          kept += 1;
          const memberType = this.checker.getTypeOfSymbol(property);
          const path = [...prefix, property.getName()];
          const callable = this.librarySignatures(memberType, 'call').length > 0;
          const node: Node = {
            member: {
              path,
              type: this.printed(memberType),
              optional: (property.flags & ts.SymbolFlags.Optional) !== 0,
              function: callable,
            },
            children: [],
          };
          into.push(node);
          if (!callable && path.length < HANDLER_MEMBER_DEPTH && this.isObjectType(memberType)) {
            next.push({ type: memberType, prefix: path, into: node.children });
          }
        }
      }
      level = next;
    }
    const members: SurfaceHandlerMember[] = [];
    const emit = (nodes: Node[]): void => {
      for (const node of nodes) {
        members.push(node.member);
        emit(node.children);
      }
    };
    emit(roots);
    const listed = { members, dropped };
    this.handlerMembers.set(type, listed);
    return listed;
  }

  private listSignatures(signatures: readonly ts.Signature[], holder: ts.Type, take: () => boolean): SurfaceSignature[] {
    const listed: SurfaceSignature[] = [];
    for (const signature of signatures) {
      if (!take()) continue;
      const params: SurfaceParam[] = [];
      const parameters = signature.getParameters();
      for (let index = 0; index < parameters.length; index++) {
        if (!take()) continue;
        const parameter = parameters[index];
        const declaration = parameter.valueDeclaration;
        const isParameter = declaration !== undefined && ts.isParameter(declaration);
        // What a claim part at this argument is checked against: a rest's
        // element, a type parameter's constraint (`keyParameterAt`).
        const slot = this.keyParameterAt(signature, index);
        const param: SurfaceParam = {
          name: parameter.getName(),
          optional: isParameter && (declaration.questionToken !== undefined || declaration.initializer !== undefined),
          rest: isParameter && declaration.dotDotDotToken !== undefined,
          type: this.printed(this.checker.getTypeOfSymbol(parameter)),
          // The checks a positional name and a positional handler pass there.
          accepts_string: slot !== undefined && this.acceptsString(this.throughConditional(slot)),
          function: this.handlerFailure(slot) === undefined,
        };
        if (slot !== undefined && slot !== VARIADIC && this.isObjectType(slot) && this.handlerFailure(slot) !== undefined) {
          const keys = this.listKeys(slot, take);
          if (keys.length > 0) param.keys = keys;
        }
        const literals = this.literalsAt(signature, { arg: index }, holder);
        if (literals.size > 0) param.literals = [...literals].sort();
        params.push(param);
      }
      const entry: SurfaceSignature = { params, returns: this.printed(this.checker.getReturnTypeOfSignature(signature)) };
      const candidates = this.listResultCandidates ? resultCandidatesOf(this.checker, signature) : undefined;
      if (candidates !== undefined) entry.result_candidates = candidates;
      listed.push(entry);
    }
    return listed;
  }

  /**
   * The keys a claim can name at an object argument, by name: every key of
   * each view the verifier reads one at a time (`keyViews`). A
   * key accepts a string, or is a handler, when it does so in some view, as
   * a keyed name or handler is checked (`viewFor`). It is optional when a
   * call can leave it out: some view marks it optional or does not declare it.
   */
  private listKeys(slot: ts.Type, take: () => boolean): SurfaceKey[] {
    const views = this.keyViews(slot);
    const keys = new Map<string, SurfaceKey & { views: number }>();
    for (const view of views) {
      for (const property of this.namedProperties(view)) {
        const type = this.checker.getTypeOfSymbol(property);
        const read = {
          name: property.getName(),
          optional: (property.flags & ts.SymbolFlags.Optional) !== 0,
          accepts_string: this.acceptsString(this.throughConditional(type)),
          function: this.handlerFailure(type) === undefined,
          views: 1,
        };
        const seen = keys.get(read.name);
        if (!seen) {
          keys.set(read.name, read);
          continue;
        }
        seen.optional = seen.optional || read.optional;
        seen.accepts_string = seen.accepts_string || read.accepts_string;
        seen.function = seen.function || read.function;
        seen.views += 1;
      }
    }
    return [...keys.values()]
      .sort((a, b) => byName(a.name, b.name))
      .filter(() => take())
      .map(({ views: declaredIn, ...key }) => ({ ...key, optional: key.optional || declaredIn < views.length }));
  }

  /**
   * A type as the declarations print it, with the service root written as
   * `<root>` (the checker spells a type no entry exports through the file that
   * declares it), cut to a length a listing can carry. The root goes first,
   * so where the cut falls does not depend on where the package is installed.
   */
  private printed(type: ts.Type): string {
    const known = this.printedTexts.get(type);
    if (known !== undefined) return known;
    surfaceReadCounts.typePrints += 1;
    const node = this.checker.typeToTypeNode(type, undefined, PRINT_FLAGS);
    const text = node === undefined ? this.checker.typeToString(type) : printTypeNode(this.orderUnions(node));
    const listed = truncate(this.scrubbed(text));
    this.printedTexts.set(type, listed);
    return listed;
  }

  /** `text` with the service root written as `<root>`. */
  private scrubbed(text: string): string {
    for (const root of this.printedRoots) text = text.split(root).join('<root>');
    return text;
  }

  /**
   * `node` with the members of every union in it, at every depth (inside a
   * function or a conditional type too), sorted by their own print with the
   * root scrubbed, compared by UTF-16 code unit. The compiler prints a union
   * in the order it made the members, which moves with whatever the program
   * read first.
   */
  private orderUnions(node: ts.TypeNode): ts.TypeNode {
    const result = ts.transform(node, [
      context => root => {
        const visit = (child: ts.Node): ts.Node => {
          const next = ts.visitEachChild(child, visit, context);
          if (!ts.isUnionTypeNode(next)) return next;
          const keyed = next.types.map(member => ({ member, key: this.scrubbed(printTypeNode(member)) }));
          keyed.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
          return ts.factory.updateUnionTypeNode(next, ts.factory.createNodeArray(keyed.map(({ member }) => member)));
        };
        return ts.visitNode(root, visit) as ts.TypeNode;
      },
    ]);
    const [ordered] = result.transformed;
    result.dispose();
    return ordered;
  }

  // --------------------------------------------------------------------------
  // Definitions
  // --------------------------------------------------------------------------

  /**
   * A declared property `name` of `type` whose type has a call signature. A
   * member typed `any`, `unknown` or `{}` says nothing (`member_untyped`).
   */
  private callableProperty(type: ts.Type, name: string): Callable {
    const property = this.declaredProperty(type, name);
    if (!property) return { failure: failed('member_missing') };
    return this.callSignatures(this.checker.getTypeOfSymbol(property));
  }

  /**
   * The call signatures the library declares. A service `declare module`
   * block can add an overload to a library member; that signature is the
   * service's claim about the library, not the library's.
   */
  private callSignatures(type: ts.Type): Callable {
    const signatures = this.checker
      .getNonNullableType(type)
      .getCallSignatures()
      .filter(signature => {
        const declaration = signature.getDeclaration();
        return declaration !== undefined && this.isLibraryFile(declaration.getSourceFile());
      });
    if (signatures.length > 0) return { signatures, holder: type };
    return { failure: this.slotFailure(type, 'member_not_callable') };
  }

  /**
   * A declared property of `type`: one the checker lists on its apparent type
   * with null and undefined removed, and not one every value of that kind
   * inherits (`constructor`, `toString`, a primitive wrapper's members). An
   * index signature does not count. Nor does a property typed `never` (or
   * only `undefined`): it cannot be passed. And a library declares it: at
   * least one declaration sits in an installed package or the default
   * library, so a member only the service's own module augmentation adds is
   * not the package's.
   */
  private declaredProperty(slot: Slot, name: string): ts.Symbol | undefined {
    return this.namedProperties(slot).find(property => property.getName() === name);
  }

  /**
   * The property `name` of a parameter a key is looked for on. It is the
   * declared property of the whole type; failing that, for a union, one that an
   * object constituent declares and whose type passes `accepts`. A constituent
   * counts only when it is an object type that says something and is not a
   * function type, so defaults written as `Options | ((parent) => Options)`
   * are read through `Options`. When a constituent declares the name but no
   * declaration passes `accepts`, that property is returned for the caller to
   * reject.
   */
  private keyProperty(slot: Slot, name: string, accepts: (type: ts.Type) => boolean): ts.Symbol | undefined {
    const whole = this.declaredProperty(slot, name);
    if (whole || slot === VARIADIC) return whole;
    let declaredOnly: ts.Symbol | undefined;
    for (const part of this.parts(slot)) {
      if (!this.isPlainObject(part)) continue;
      const property = this.declaredProperty(part, name);
      if (!property) continue;
      if (accepts(this.checker.getTypeOfSymbol(property))) return property;
      declaredOnly ??= property;
    }
    return declaredOnly;
  }

  /**
   * An object type that is not a function type. (One that says nothing
   * declares no property, so it never supplies a key.)
   */
  private isPlainObject(type: ts.Type): boolean {
    return (
      this.isObjectLike(type) &&
      type.getCallSignatures().length === 0 &&
      type.getConstructSignatures().length === 0
    );
  }

  /**
   * A parameter a key is looked for on, or that must accept a string: read
   * through a rest parameter's element and through a type parameter's
   * constraint. A type parameter with no constraint, or one of `any` or
   * `unknown`, stays as it is and says nothing.
   */
  private keyParameterAt(signature: ts.Signature, index: number): Slot | undefined {
    const slot = this.parameterAt(signature, index, true);
    return slot === undefined ? undefined : this.throughConstraint(slot);
  }

  private throughConstraint(slot: Slot): Slot {
    if (slot === VARIADIC) return slot;
    const parts = this.parts(slot);
    if (parts.length !== 1 || !(parts[0].flags & ts.TypeFlags.TypeParameter)) return slot;
    // An `any` or `unknown` constraint says nothing, as the bare parameter does.
    return this.checker.getBaseConstraintOfType(parts[0]) ?? slot;
  }

  /**
   * The declared properties a claim can name: every one keyed by a string. A
   * member or key keyed by a unique symbol (`[Symbol.iterator]`, the runtime
   * emitter's `[captureRejectionSymbol]`) has no name a claim can carry, and
   * the checker names it with a symbol id that moves whenever anything
   * earlier in the process does (`__@iterator@84`): it is never listed, and
   * it neither satisfies nor blocks a slot or a name (D2).
   */
  private namedProperties(slot: Slot): ts.Symbol[] {
    return this.declaredProperties(slot).filter(property => !isSymbolKeyed(property));
  }

  private declaredProperties(slot: Slot): ts.Symbol[] {
    if (slot === VARIADIC) return [];
    const cached = this.declared.get(slot);
    if (cached) return cached;
    const apparent = this.checker.getApparentType(this.checker.getNonNullableType(slot));
    const declared = this.checker
      .getPropertiesOfType(apparent)
      .filter(
        property =>
          !this.isBuiltinMember(property) &&
          this.isLibraryDeclared(property, apparent) &&
          !this.isAbsent(property)
      );
    this.declared.set(slot, declared);
    return declared;
  }

  /**
   * Some declaration of the property is in an installed package or the
   * default library. A member a mapped type makes (`extends Record<'get',
   * Fn>`) has no declaration of its own; it counts when the type listing it
   * was written by the library.
   */
  private isLibraryDeclared(property: ts.Symbol, listing: ts.Type): boolean {
    const declarations = property.declarations ?? [];
    if (declarations.length === 0) {
      return (
        !this.isNamedByServiceAugmentation(property.getName()) &&
        this.isListedByLibraryType(listing, property.getName(), file => this.isLibraryFile(file))
      );
    }
    return declarations.some(declaration => this.isLibraryFile(declaration.getSourceFile()));
  }

  /** Judge the next check as a claim about `pkg`. */
  setPackage(pkg: string): void {
    const named = packageNameOf(pkg);
    if (named !== this.currentPackage) {
      this.declared.clear();
      this.handlerMembers.clear();
    }
    this.currentPackage = named;
  }

  /**
   * The service's own `declare module '<this package>'` (or one of its
   * subpaths) or `declare global` blocks declare a member of this name,
   * compared without case. A mapped type's member has no declaration of its
   * own, so when the service adds a key to the interface a library mapped
   * type iterates (`Record<keyof MethodMap, Fn>`, with or without `& string`,
   * or re-cased by an `as Lowercase<...>` remap), nothing on the member says
   * the service put it there. Blocks for other packages do not count: a
   * service augments many packages, and their member names say nothing about
   * this one.
   */
  private isNamedByServiceAugmentation(name: string): boolean {
    const names = this.serviceAugmentedNames();
    const lower = name.toLowerCase();
    return Boolean(names.get(this.currentPackage)?.has(lower) || names.get('global')?.has(lower));
  }

  private serviceAugmentedNames(): ReadonlyMap<string, ReadonlySet<string>> {
    if (this.augmentedNames) return this.augmentedNames;
    const byPackage = new Map<string, Set<string>>();
    const collect = (node: ts.Node, names: Set<string>): void => {
      if (
        (ts.isPropertySignature(node) ||
          ts.isMethodSignature(node) ||
          ts.isPropertyDeclaration(node) ||
          ts.isMethodDeclaration(node) ||
          ts.isEnumMember(node)) &&
        (ts.isIdentifier(node.name) || ts.isStringLiteral(node.name) || ts.isNumericLiteral(node.name))
      ) {
        names.add(node.name.text.toLowerCase());
      }
      ts.forEachChild(node, child => collect(child, names));
    };
    for (const file of this.program.getSourceFiles()) {
      if (file === this.probe || this.isLibraryFile(file)) continue;
      for (const statement of file.statements) {
        if (!ts.isModuleDeclaration(statement) || !statement.body) continue;
        const key = ts.isStringLiteral(statement.name)
          ? packageNameOf(statement.name.text)
          : statement.flags & ts.NodeFlags.GlobalAugmentation
            ? 'global'
            : undefined;
        if (key === undefined) continue;
        let names = byPackage.get(key);
        if (!names) byPackage.set(key, (names = new Set<string>()));
        collect(statement.body, names);
      }
    }
    this.augmentedNames = byPackage;
    return byPackage;
  }

  /**
   * Member `name`, which has no declaration of its own, reaches `listing`
   * along a path of types `isAllowed` files alone declare. The path runs
   * through the types that list the member: an intersection's parts
   * (`type Client = {...} & Record<Alias, Fn> & Fn`), and an interface's base
   * types (`interface S extends Base`), each declared only in allowed files.
   * The library's `Record` is; a base interface the service extended with its
   * own mapped type is not, at any depth.
   */
  private isListedByLibraryType(
    listing: ts.Type,
    name: string,
    isAllowed: (file: ts.SourceFile) => boolean
  ): boolean {
    const listedBy = (types: readonly ts.Type[]) =>
      types.some(part => {
        const apparent = this.checker.getApparentType(part);
        return (
          this.checker.getPropertyOfType(apparent, name) !== undefined &&
          this.isListedByLibraryType(apparent, name, isAllowed)
        );
      });
    if (listing.isIntersection()) return listedBy(listing.types);
    if (!this.isDeclaredOnlyIn(listing.getSymbol(), isAllowed)) return false;
    const objectFlags = (listing as ts.ObjectType).objectFlags ?? 0;
    const target =
      objectFlags & ts.ObjectFlags.Reference ? (listing as ts.TypeReference).target : listing;
    const targetFlags = (target as ts.ObjectType).objectFlags ?? 0;
    // A type literal or mapped type lists its members itself.
    if (!(targetFlags & ts.ObjectFlags.ClassOrInterface)) return true;
    return listedBy(this.checker.getBaseTypes(target as ts.InterfaceType));
  }

  private isDeclaredOnlyIn(symbol: ts.Symbol | undefined, isAllowed: (file: ts.SourceFile) => boolean): boolean {
    const declarations = symbol?.declarations ?? [];
    return declarations.length > 0 && declarations.every(declaration => isAllowed(declaration.getSourceFile()));
  }

  /** An installed package's file, or the default library's. */
  private isLibraryFile(file: ts.SourceFile): boolean {
    return this.program.isSourceFileDefaultLibrary(file) || this.isInstalledFile(file.fileName);
  }

  private isInstalledFile(fileName: string): boolean {
    return this.installedPackage(fileName) !== undefined;
  }

  /**
   * The installed package a file belongs to. Its path, taken relative to the
   * service root, has a package directory after its last `node_modules`
   * segment (two segments for a scope), holding a package.json. A service
   * source file in a directory that happens to be named `node_modules`, or a
   * repository checked out under a `node_modules` ancestor, is not installed.
   * An install hoisted above the service root (`../../node_modules/pkg`) and
   * a pnpm store (`node_modules/.pnpm/pkg@1/node_modules/pkg`) are. A path
   * with no `node_modules` segment is placed by the module graph, where there
   * is one (carrick#2599).
   * Mirrored by `installedPackageDirectory` in capture/, which the capture
   * seam keeps from being shared.
   */
  private installedPackage(fileName: string): InstalledPackage | undefined {
    const real = this.realpath(fileName);
    const segments = path.relative(this.root, real).split(path.sep);
    const last = segments.lastIndexOf('node_modules');
    let placed: { directory: string; name: string } | undefined;
    if (last >= 0) {
      const width = segments[last + 1]?.startsWith('@') ? 2 : 1;
      placed = {
        directory: path.resolve(this.root, ...segments.slice(0, last + 1 + width)),
        name: segments.slice(last + 1, last + 1 + width).join('/'),
      };
    } else {
      placed = this.graph.packageDirectoryOf(real);
    }
    if (placed === undefined) return undefined;
    // No package.json there (a file directly under node_modules included) means no package.
    let known = this.packageDirectories.get(placed.directory);
    if (known === undefined) {
      known = readInstalledPackage(this.host, placed.directory, placed.name);
      this.packageDirectories.set(placed.directory, known);
    }
    return known ?? undefined;
  }

  /** Typed so that nothing can be passed: `never`, or only `undefined`. */
  private isAbsent(property: ts.Symbol): boolean {
    return this.parts(this.checker.getTypeOfSymbol(property)).every(
      part => (part.flags & ts.TypeFlags.Never) !== 0
    );
  }

  private isBuiltinMember(property: ts.Symbol): boolean {
    const declarations = property.declarations ?? [];
    return (
      declarations.length > 0 &&
      declarations.every(declaration => {
        const owner = declaration.parent;
        if (!owner || !ts.isInterfaceDeclaration(owner)) return false;
        const symbol = this.checker.getSymbolAtLocation(owner.name);
        return symbol !== undefined && BUILTIN_INTERFACES.has(this.checker.getFullyQualifiedName(symbol));
      })
    );
  }

  /**
   * `string` is assignable to the type, and some part of it besides null and
   * undefined is string-like. `any`, `unknown`, `{}` and `Object` accept a
   * string without saying anything about one.
   */
  private acceptsString(declared: Slot): boolean {
    const slot = this.throughConstraint(declared);
    if (slot === VARIADIC) return false;
    return (
      this.checker.isTypeAssignableTo(this.checker.getStringType(), slot) &&
      this.parts(slot).some(part => isStringLike(part))
    );
  }

  /** Accepts string, or names an HTTP method as a literal in either case. */
  private acceptsMethod(declared: Slot): boolean {
    const slot = this.throughConstraint(declared);
    if (slot === VARIADIC) return false;
    if (this.acceptsString(slot)) return true;
    return this.parts(slot).some(
      part => part.isStringLiteral() && HTTP_METHODS.has(asciiUpperCase(part.value))
    );
  }

  /**
   * A body parameter that takes any payload: `unknown`, or a type parameter
   * with no constraint (or one of `unknown` or `any`). `any` itself is not
   * open: a declared `any`, an unresolved type and a defaulted type argument
   * all read as `any`, and none of them says the parameter is a body.
   */
  private isOpenBody(slot: Slot): boolean {
    if (slot === VARIADIC) return false;
    const parts = this.parts(slot);
    return (
      parts.length > 0 &&
      parts.every(part => (part.flags & ts.TypeFlags.Unknown) !== 0 || this.isUnconstrained(part))
    );
  }

  /** Every part besides null and undefined is an object type. */
  private isObjectType(slot: Slot): boolean {
    if (slot === VARIADIC) return false;
    const parts = this.parts(slot);
    return parts.length > 0 && parts.every(part => this.isObjectLike(part));
  }

  private isObjectLike(type: ts.Type): boolean {
    if (type.flags & ts.TypeFlags.Object) return true;
    if (type.isIntersection()) return type.types.every(part => this.isObjectLike(part));
    if (type.flags & ts.TypeFlags.TypeParameter) {
      const constraint = this.checker.getBaseConstraintOfType(type);
      return constraint !== undefined && constraint !== type && this.isObjectLike(constraint);
    }
    return false;
  }

  /**
   * The verdict when a slot fails its predicate: `unchecked` (`member_untyped`)
   * when its type says nothing, `failed` with `code` when it says something
   * else.
   */
  private slotFailure(slot: Slot, code: string): Outcome {
    return this.saysNothing(slot) ? unchecked('member_untyped') : failed(code);
  }

  /**
   * Some part of the type, besides null and undefined, is `any`, `unknown`,
   * an unconstrained type parameter, or an object type with nothing declared
   * on it (`{}`, `Object`, `Function`).
   */
  private saysNothing(slot: Slot): boolean {
    if (slot === VARIADIC) return true;
    return this.parts(slot).some(
      part => this.isOpenTop(part) || this.isUnconstrained(part) || this.isEmptyObject(part)
    );
  }

  /**
   * What a factory builds says nothing: the type itself, or any branch of a
   * conditional return type, does. A conditional whose branches lead back to
   * itself (a recursive alias) is read once: `seen` holds the conditionals
   * on the way down, and one met again says nothing new.
   */
  returnSaysNothing(type: ts.Type, seen: Set<ts.Node> = new Set()): boolean {
    if (this.saysNothing(type)) return true;
    return this.parts(type).some(part => {
      if (!(part.flags & ts.TypeFlags.Conditional)) return false;
      const node = (part as ts.ConditionalType).root.node;
      if (seen.has(node)) return false;
      seen.add(node);
      return [node.trueType, node.falseType].some(branch =>
        this.returnSaysNothing(this.checker.getTypeFromTypeNode(branch), seen)
      );
    });
  }

  /** `any` (an unresolved type included) or `unknown`. */
  private isOpenTop(type: ts.Type): boolean {
    return (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) !== 0;
  }

  private isUnconstrained(type: ts.Type): boolean {
    if (!(type.flags & ts.TypeFlags.TypeParameter)) return false;
    const constraint = this.checker.getBaseConstraintOfType(type);
    return constraint === undefined || this.isOpenTop(constraint);
  }

  private isEmptyObject(type: ts.Type): boolean {
    return (
      (type.flags & ts.TypeFlags.Object) !== 0 &&
      this.declaredProperties(type).length === 0 &&
      type.getCallSignatures().length === 0 &&
      type.getConstructSignatures().length === 0 &&
      this.checker.getIndexInfosOfType(type).length === 0
    );
  }

  /** The type's parts besides null and undefined. */
  private parts(type: ts.Type): readonly ts.Type[] {
    return (type.isUnion() ? type.types : [type]).filter(part => !isNullish(part));
  }

  /**
   * What the signature has at parameter `index`, reading through a rest
   * parameter; `undefined` when it has none there. A rest typed by a type
   * parameter (`...rest: A`) is unreadable, unless `readConstraint` asks for
   * its element through the constraint (`A extends Array<X>` reads `X`).
   */
  private parameterAt(signature: ts.Signature, index: number, readConstraint = false): Slot | undefined {
    const parameters = signature.getParameters();
    const last = parameters[parameters.length - 1];
    const declaration = last?.valueDeclaration;
    const restIndex =
      declaration && ts.isParameter(declaration) && declaration.dotDotDotToken
        ? parameters.length - 1
        : -1;
    if (restIndex === -1 || index < restIndex) {
      return index < parameters.length ? this.checker.getTypeOfSymbol(parameters[index]) : undefined;
    }
    let rest = this.checker.getTypeOfSymbol(last);
    if (readConstraint && rest.flags & ts.TypeFlags.TypeParameter) {
      rest = this.checker.getBaseConstraintOfType(rest) ?? rest;
    }
    if (this.checker.isTupleType(rest)) {
      return this.checker.getTypeArguments(rest as ts.TypeReference)[index - restIndex];
    }
    if (this.checker.isArrayType(rest)) {
      return this.checker.getTypeArguments(rest as ts.TypeReference)[0];
    }
    return this.isOpenTop(rest) ? rest : VARIADIC;
  }
}

/**
 * What `typeToString` asks the node builder for, less its length cut: a
 * union the builder cuts short keeps the members it made first, so the
 * listing orders every union before its own cut (`truncate`).
 */
/** The longest member path a handler parameter's members are listed to (`['raw', 'url']`). */
const HANDLER_MEMBER_DEPTH = 2;
/**
 * The most members one handler parameter lists (carrick#2966), top-level
 * first. The store's server question shows at most 300 of them in the same
 * order, so this must stay at least that.
 */
const HANDLER_MEMBER_CAP = 500;

/**
 * The most handler-parameter members one `listSurface` call lists, across all
 * its specifiers (carrick#3016). The answer is one JSON line and a string
 * passes V8's limit at about 512 MiB; a member costs about 150 bytes, so this
 * keeps the handlers near 75 MB beside an export listing of tens of MB. A
 * server framework handing 50 distinct request types to thousands of members
 * listed 8.99 million of them (1.3 GB) before it.
 */
const HANDLER_LISTING_BUDGET = 500_000;

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
}

const PRINT_FLAGS =
  ts.NodeBuilderFlags.AllowUniqueESSymbolType |
  ts.NodeBuilderFlags.UseAliasDefinedOutsideCurrentScope |
  ts.NodeBuilderFlags.IgnoreErrors |
  ts.NodeBuilderFlags.NoTruncation;

const TYPE_PRINTER = ts.createPrinter({ removeComments: true });

/**
 * A type node as `typeToString` prints it with no enclosing declaration: with
 * no source file, so a node the builder reused from a declaration prints its
 * own text.
 */
function printTypeNode(node: ts.Node): string {
  return TYPE_PRINTER.printNode(ts.EmitHint.Unspecified, node, undefined as unknown as ts.SourceFile);
}

/** The type exports a listing builds with the probe: the type-only ones, and with `types: true` the exported classes (carrick#3050). */
function typeNamesOf(read: { types: readonly string[]; classes: readonly string[] }, options: SurfaceOptions): string[] {
  return options.types === true ? [...read.types, ...read.classes].sort(byName) : [...read.types];
}

/** Code-unit order on names. */
/** Every type parameter of an interface, type alias or class has a default, in some declaration of it (carrick#2977). */
function typeParametersDefaulted(symbol: ts.Symbol): boolean {
  const declarations = (symbol.declarations ?? []).filter(
    (declaration): declaration is ts.InterfaceDeclaration | ts.TypeAliasDeclaration | ts.ClassDeclaration =>
      ts.isInterfaceDeclaration(declaration) || ts.isTypeAliasDeclaration(declaration) || ts.isClassDeclaration(declaration)
  );
  if (declarations.length === 0) return false;
  const count = Math.max(...declarations.map(declaration => declaration.typeParameters?.length ?? 0));
  for (let index = 0; index < count; index++) {
    if (!declarations.some(declaration => declaration.typeParameters?.[index]?.default !== undefined)) return false;
  }
  return true;
}

function byName(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

/**
 * Properties by name. A type's own property order is declaration order, but
 * a mapped type's (`Record<keyof M, F>`, `Partial<A & B>`) follows its key
 * union, in the order the compiler made the keys: listed as it comes, the
 * order would move with whatever the program read first.
 */
function sortedByName(properties: readonly ts.Symbol[]): ts.Symbol[] {
  return [...properties].sort((a, b) => byName(a.getName(), b.getName()));
}

/** A printed type, cut to a length a listing can carry. */
function truncate(text: string): string {
  return text.length > 200 ? `${text.slice(0, 197)}...` : text;
}

/**
 * The package a specifier names (`@scope/name` or `name`, without a subpath)
 * is `packageName`, or `packageName` is its `@types` package
 * (`@types/scope__name` for a scoped one).
 */
function isNamedPackage(specifier: string, packageName: string | undefined): boolean {
  const named = packageNameOf(specifier);
  return packageName === named || packageName === typesPackageOf(named);
}

/** The `@types` package of a package name (`@types/scope__name` for a scoped one). */
function typesPackageOf(named: string): string {
  return `@types/${named.startsWith('@') ? named.slice(1).replace('/', '__') : named}`;
}

/** The package a specifier names: `@scope/name` or `name`, without a subpath. */
function packageNameOf(specifier: string): string {
  const segments = specifier.split('/');
  return specifier.startsWith('@') ? segments.slice(0, 2).join('/') : segments[0];
}

/** A package directory under `node_modules`: the name it is installed as, and the name and version its package.json gives. */
interface InstalledPackage {
  directoryName: string;
  packageName: string | undefined;
  version: string | undefined;
}

function readInstalledPackage(
  host: ts.ModuleResolutionHost,
  directory: string,
  directoryName: string
): InstalledPackage | null {
  const manifest = path.join(directory, 'package.json');
  if (!host.fileExists(manifest)) return null;
  let packageName: string | undefined;
  let version: string | undefined;
  try {
    const parsed = JSON.parse(host.readFile(manifest) ?? '') as { name?: unknown; version?: unknown } | null;
    if (typeof parsed?.name === 'string') packageName = parsed.name;
    if (typeof parsed?.version === 'string') version = parsed.version;
  } catch {
    // An unreadable manifest still marks an installed directory; it names no package.
  }
  return { directoryName, packageName, version };
}

function isNullish(type: ts.Type): boolean {
  return (type.flags & (ts.TypeFlags.Null | ts.TypeFlags.Undefined | ts.TypeFlags.Void)) !== 0;
}

/**
 * Keyed by a unique symbol. The checker escapes such a name as
 * `__@<description>@<symbol id>`; a string key that starts with `__` is
 * escaped with one more underscore, so it never matches.
 */
function isSymbolKeyed(property: ts.Symbol): boolean {
  return (property.escapedName as string).startsWith('__@');
}

/** `string`, a string literal or template, or an intersection with one (`string & {}`). */
function isStringLike(type: ts.Type): boolean {
  if (type.flags & ts.TypeFlags.StringLike) return true;
  return type.isIntersection() && type.types.some(isStringLike);
}
