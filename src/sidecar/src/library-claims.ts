/**
 * `verify_library_claims` (carrick#1616, prototype) and
 * `verify_client_semantics` (carrick#1564): check what a model said about a
 * library against the package's own type declarations.
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
 * the design's must-not-verify rules (a member another package declares, a
 * name slot with an unassigned string sibling, a handler that says nothing).
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
import type {
  ClaimSlot,
  ClaimVariant,
  LibraryCheck,
  LibraryClaim,
  LibraryRole,
  LibrarySurface,
  SemanticsCheck,
  SemanticsModule,
  SemanticsResult,
  SurfaceExport,
  SurfaceKey,
  SurfaceMember,
  SurfaceParam,
  SurfaceReceiver,
  SurfaceSignature,
} from './types.js';

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
 * The runtime's own type packages. A type they declare (the runtime's event
 * emitter above all) is never part of another package's own declarations,
 * even when that package's maker returns it: a member only the runtime
 * declares is the runtime's, not the package's. When the named package is
 * one of these, it is its own.
 */
const RUNTIME_TYPE_PACKAGES: ReadonlySet<string> = new Set([
  '@types/node',
  '@types/bun',
  'bun-types',
  '@types/deno',
]);

/** A dependency range that names the service's own source, not a registry release. */
const LOCAL_RANGE = /^(workspace|file|link|portal):/;

/** A resolved module lands on TypeScript: a declaration file or source. */
const TYPESCRIPT_FILE = /\.(d\.[mc]?ts|[mc]?tsx?)$/;

const IDENTIFIER = /^[A-Za-z_$][\w$]*$/;

/** The HTTP receivers of #1564: the export, or what one factory member returns. */
const HTTP_RECEIVER = /^(export|instance:\S+)$/;

type MakesClaim = Extract<LibraryClaim, { list: 'makes' }>;
type ScopesClaim = Extract<LibraryClaim, { list: 'scopes' }>;
type OpsClaim = Extract<LibraryClaim, { list: 'ops' }>;
type ReservedClaim = Extract<LibraryClaim, { list: 'reserved' }>;
type RequestArgs = 'config' | 'path_options';

/** Which checks a role's claims need. */
interface RoleRules {
  /** The #1564 HTTP checks, with their reasons. */
  http: boolean;
  /**
   * The message checks: a member must be the package's own (not inherited
   * from another package's type), a name slot must be the only unassigned
   * string slot of its call and not a key of an index-signature map, a
   * handler must declare a signature, an instance must come from a maker
   * claim that holds, and a workspace or `file:` dependency is not checked.
   */
  message: boolean;
  /** The op kinds it can check. */
  ops: ReadonlySet<string>;
}

const HTTP_RULES: RoleRules = { http: true, message: false, ops: new Set(['request']) };
const MESSAGE_RULES: RoleRules = {
  http: false,
  message: true,
  ops: new Set(['send', 'receive', 'request']),
};

/**
 * The role table: the role alone picks the checks. A role the slice does not
 * read yet (GraphQL executors, server mounts) answers `role_unsupported`, and
 * `none` has nothing on the wire to check.
 */
const ROLE_TABLE: Readonly<Record<LibraryRole, RoleRules | undefined>> = {
  http_client: HTTP_RULES,
  broker: MESSAGE_RULES,
  in_process_bus: MESSAGE_RULES,
  socket: MESSAGE_RULES,
  graphql_client: undefined,
  server_framework: undefined,
  none: undefined,
};

/** How one check came out, before it is stamped with its claim and receiver. */
type Outcome =
  | { verdict: 'verified' }
  | { verdict: 'failed' | 'unchecked'; reason: string };

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
type Callable = { signatures: readonly ts.Signature[] } | { failure: Outcome };

interface ModuleRead {
  entry: SemanticsModule;
  /** The reason every check on this package is unchecked, when there is one. */
  reason?: string;
}

interface ExportRead {
  type: ts.Type;
  /** The export's own declaration, at the end of its re-export chain. */
  target: ts.Symbol;
}

/**
 * A receiver a message claim is read on: its type, and the packages whose
 * declarations are its own (the named package, the packages its export is
 * re-exported from, and the package that declares the receiver's type).
 */
interface MessageReceiver {
  type: ts.Type;
  home: ReadonlySet<string>;
  /** A maker claim for this instance binds a name (`{ bound: 'maker' }` reads it). */
  makerBindsName: boolean;
  /** The receiver came through a scope member (`{ bound: 'scope' }` reads it). */
  scoped: boolean;
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
  scope?: string;
}

export interface VerifyResult {
  semantics: SemanticsResult[];
  modules: SemanticsModule[];
}

/**
 * A #1564 HTTP check in the shared shape. The conversion is one to one, so
 * each claim keeps its own id and its own verdict:
 * - `factory` is a `makes` call of `member` with the base key at argument 0;
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
        claim: { list: 'makes', form: 'call', member: claim.member, base: { arg: 0, key: claim.base_url_key } },
      };
    case 'verb':
      return {
        ...common,
        claim: { list: 'ops', op: 'request', member: claim.member, method: claim.method, name: { arg: 0 } },
      };
    case 'verb_body':
      return {
        ...common,
        claim:
          claim.args === 'path_body'
            ? { list: 'ops', op: 'request', member: claim.member, name: { arg: 0 }, payload: { arg: 1 } }
            : {
                list: 'ops',
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
          list: 'ops',
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

let probeSequence = 0;

export class LibraryClaimsVerifier {
  constructor(private readonly project: Project) {}

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
    variants: ReadonlySet<ClaimVariant> = new Set()
  ): VerifyResult {
    const deadline = performance.now() + budgetMs;
    const stamp = (check: LibraryCheck, outcome: Outcome): SemanticsResult =>
      outcome.verdict === 'verified'
        ? { claim_id: check.claim_id, receiver: check.receiver, verdict: 'verified' }
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
      if (check.role === 'http_client' && claim.list === 'makes' && claim.member !== null && claim.base?.key) {
        const factory = factoryKey(key, claim.member);
        const keys = factoryKeys.get(factory) ?? new Set<string>();
        keys.add(claim.base.key);
        factoryKeys.set(factory, keys);
      }
    }
    const probeText = importKeys
      .map((key, i) => {
        const [pkg, name] = JSON.parse(key) as [string, string];
        return importLine(pkg, name, `__carrick_e${i}`);
      })
      .join('\n');

    const probePath = path.join(fromDir, `__carrick_claims_probe_${process.pid}_${probeSequence++}.ts`);
    const probe = this.project.createSourceFile(probePath, `${probeText}\n`, { overwrite: true });
    try {
      const program = this.project.getProgram().compilerObject;
      const file = program.getSourceFile(probe.getFilePath());
      if (!file) throw new Error(`probe file ${probePath} is not in the program`);
      const reader = new DeclarationReader(program, file, this.project.getModuleResolutionHost(), fromDir, variants);
      const localRanges = readLocalRanges(fromDir);

      const moduleReads = new Map<string, ModuleRead>();
      const exportReads = new Map<string, Read<ExportRead>>();
      const httpReceivers = new Map<string, Read<ts.Type>>();
      const judged = new Map<number, Judged>();

      const declarationOf = (check: LibraryCheck) =>
        file.statements[importIndex.get(exportKey(check))!] as ts.ImportDeclaration;

      const readModule = (check: LibraryCheck): ModuleRead => {
        let moduleRead = moduleReads.get(check.package);
        if (!moduleRead) {
          moduleRead = reader.readModule(check.package, declarationOf(check));
          moduleReads.set(check.package, moduleRead);
        }
        return moduleRead;
      };

      const readExport = (check: LibraryCheck): Read<ExportRead> => {
        const key = exportKey(check);
        let exportRead = exportReads.get(key);
        if (!exportRead) {
          exportRead = reader.readExport(declarationOf(check));
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
        const receiverPath = parseReceiver(check.receiver);
        if (!receiverPath) return { reason: 'receiver_invalid' };
        let types: readonly ts.Type[] = [exported.type];
        let makerBindsName = false;
        if (receiverPath.maker) {
          const maker = receiverPath.maker;
          const makers = checks
            .map((other, i) => ({ other, i }))
            .filter(
              ({ other }) =>
                other.package === check.package &&
                other.export === check.export &&
                other.receiver === 'export' &&
                other.claim.list === 'makes' &&
                other.claim.form === maker.form &&
                other.claim.member === maker.member
            );
          const made = holdingReturns(makers.map(({ i }) => judge(i)));
          if ('reason' in made) return { reason: made.reason === 'unresolved' ? 'maker_unresolved' : 'maker_unverified' };
          types = made.value;
          makerBindsName = makers.some(({ other }) => other.claim.list === 'makes' && other.claim.name !== undefined);
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
                other.claim.list === 'scopes' &&
                other.claim.member === scope
            );
          const scoped = holdingReturns(scopes.map(({ i }) => judge(i)));
          if ('reason' in scoped) return { reason: scoped.reason === 'unresolved' ? 'scope_unresolved' : 'scope_unverified' };
          types = scoped.value;
        }
        if (types.some(type => reader.returnSaysNothing(type))) {
          return { reason: receiverPath.maker ? 'maker_unresolved' : 'export_untyped' };
        }
        return {
          value: types.map(type => ({
            type,
            home: reader.homeOf(check.package, [exported.target, ...typeSymbols(type)]),
            makerBindsName,
            scoped: receiverPath.scope !== undefined,
          })),
        };
      };

      /** What a set of maker (or scope) judgements build: the distinct types their common overloads return. */
      const holdingReturns = (judgements: Judged[]): Read<ts.Type[]> => {
        if (judgements.length === 0) return { reason: 'unverified' };
        let holding: readonly ts.Signature[] | undefined;
        for (const judgement of judgements) {
          if (judgement.outcome.verdict !== 'verified' || !judgement.holding) return { reason: 'unverified' };
          holding = holding === undefined ? judgement.holding : holding.filter(sig => judgement.holding!.includes(sig));
        }
        const returns = [...new Set((holding ?? []).map(sig => reader.returnOf(sig)))];
        if (returns.length === 0) return { reason: 'unresolved' };
        return { value: returns };
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
        const rules = ROLE_TABLE[check.role];
        if (!rules) return { outcome: unchecked('role_unsupported') };
        // An export given two roles is classified two ways; neither is a fact.
        if ((roles.get(exportKey(check))?.size ?? 0) > 1) return { outcome: unchecked('role_conflict') };
        if (rules.http && !HTTP_RECEIVER.test(check.receiver)) return { outcome: unchecked('receiver_invalid') };
        if (rules.message && !parseReceiver(check.receiver)) return { outcome: unchecked('receiver_invalid') };
        reader.setPackage(check.package);

        const moduleRead = readModule(check);
        if (moduleRead.reason) return { outcome: unchecked(moduleRead.reason) };
        if (rules.message && LOCAL_RANGE.test(localRanges.get(packageNameOf(check.package)) ?? '')) {
          return { outcome: unchecked('module_workspace') };
        }
        const exportRead = readExport(check);
        if ('reason' in exportRead) return { outcome: unchecked(exportRead.reason) };

        if (rules.http) {
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

        if (check.claim.list === 'ops' && !rules.ops.has(check.claim.op)) {
          return { outcome: unchecked('role_unsupported') };
        }
        const receivers = readMessageReceivers(index, exportRead.value);
        if ('reason' in receivers) return { outcome: unchecked(receivers.reason) };
        const holding: ts.Signature[] = [];
        for (const receiver of receivers.value) {
          const result = reader.judgeMessage(receiver, check.claim);
          if (result.outcome.verdict !== 'verified') return result;
          holding.push(...(result.holding ?? []));
        }
        return { outcome: VERIFIED, holding };
      };

      const semantics: SemanticsResult[] = [];
      for (let index = 0; index < checks.length; index++) {
        const check = checks[index];
        if (!judged.has(index) && performance.now() >= deadline) {
          semantics.push(stamp(check, unchecked('budget')));
          continue;
        }
        semantics.push(stamp(check, judge(index).outcome));
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
   * Each package's declared surface, read with the verifier's predicates:
   * every value export, the receivers a claim can be read on (the export, what
   * calling or constructing it returns, what its maker members return), and
   * each receiver's callable members with their parameter slots. Capped at
   * `maxEntries` per package, with the count dropped.
   */
  listSurface(fromDir: string, packages: string[], maxEntries: number): LibrarySurface[] {
    if (packages.length === 0) return [];
    const probeText = packages
      .map((pkg, i) => `import * as __carrick_ns${i} from ${JSON.stringify(pkg)};`)
      .join('\n');
    const probePath = path.join(fromDir, `__carrick_surface_probe_${process.pid}_${probeSequence++}.ts`);
    const probe = this.project.createSourceFile(probePath, `${probeText}\n`, { overwrite: true });
    try {
      const program = this.project.getProgram().compilerObject;
      const file = program.getSourceFile(probe.getFilePath());
      if (!file) throw new Error(`probe file ${probePath} is not in the program`);
      const reader = new DeclarationReader(program, file, this.project.getModuleResolutionHost(), fromDir);
      return packages.map((pkg, i) =>
        reader.listPackage(pkg, file.statements[i] as ts.ImportDeclaration, maxEntries)
      );
    } finally {
      this.project.removeSourceFile(probe);
    }
  }
}

function exportKey(check: { package: string; export: string }): string {
  return JSON.stringify([check.package, check.export]);
}

function factoryKey(exportKeyText: string, member: string): string {
  return `${exportKeyText}\u0000${member}`;
}

/**
 * `export`, `instance:()`, `instance:new`, `instance:new:<member>` or
 * `instance:<member>`, optionally followed by `>scope:<member>`.
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
  if (base === 'instance:()') return { base, scope, maker: { form: 'call', member: null } };
  if (base === 'instance:new') return { base, scope, maker: { form: 'new', member: null } };
  let match = /^instance:new:(\S+)$/.exec(base);
  if (match) return { base, scope, maker: { form: 'new', member: match[1] } };
  match = /^instance:(\S+)$/.exec(base);
  if (match && match[1] !== '()' && match[1] !== 'new') return { base, scope, maker: { form: 'call', member: match[1] } };
  return undefined;
}

/** The symbols that say who declared a type: its own, its alias's, its parts'. */
function typeSymbols(type: ts.Type): (ts.Symbol | undefined)[] {
  const symbols: (ts.Symbol | undefined)[] = [type.getSymbol(), type.aliasSymbol];
  if (type.isUnionOrIntersection()) {
    for (const part of type.types) symbols.push(part.getSymbol(), part.aliasSymbol);
  }
  return symbols;
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

/** Where the parts of a message claim sit, and which argument holds which keys. */
interface ClaimLayout {
  /** Positional parts by argument. */
  positional: Map<number, 'name' | 'base' | 'payload' | 'handler' | 'ack'>;
  /** Keyed parts by argument, then by key. */
  keyed: Map<number, Map<string, 'name' | 'base' | 'payload' | 'handler' | 'ack'>>;
}

type PartName = 'name' | 'base' | 'payload' | 'handler' | 'ack';

/**
 * Reads the declarations the probe imported, with the program's own checker.
 * Every predicate is the contract's (carrick#1564, section 3), stated on the
 * checker's public API, with the definitions the review amended.
 */
class DeclarationReader {
  private readonly checker: ts.TypeChecker;
  /** The service root, as a realpath. */
  private readonly root: string;
  private readonly packageDirectories = new Map<string, InstalledPackage | null>();
  private readonly realpaths = new Map<string, string>();
  /** Names the service declares in its own blocks, per augmented package (`global` for `declare global`). */
  private augmentedNames: ReadonlyMap<string, ReadonlySet<string>> | undefined;
  /** The package of the check being judged; see `serviceAugmentedNames`. */
  private currentPackage = '';

  constructor(
    private readonly program: ts.Program,
    private readonly probe: ts.SourceFile,
    private readonly host: ts.ModuleResolutionHost,
    serviceRoot: string,
    /** Readings switched on for this request; none is the strict default. */
    private readonly variants: ReadonlySet<ClaimVariant> = new Set()
  ) {
    this.checker = program.getTypeChecker();
    this.root = this.realpath(serviceRoot);
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
  readModule(pkg: string, declaration: ts.ImportDeclaration): ModuleRead {
    const specifier = declaration.moduleSpecifier as ts.StringLiteral;
    const resolved = ts.resolveModuleName(
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

  /** What a call (or construct) signature returns. */
  returnOf(signature: ts.Signature): ts.Type {
    return this.checker.getReturnTypeOfSignature(signature);
  }

  /**
   * The packages whose declarations are a receiver's own: the named package
   * (and its `@types` package), and every package that declares one of
   * `symbols` (the export at the end of its re-export chain, the receiver's
   * type, its alias, its parts). A meta-package that re-exports a scoped core
   * package's client owns that client's members; a class that extends another
   * package's base does not own what it only inherits. The runtime's type
   * packages and the default library never join, unless the named package is
   * one of them.
   */
  homeOf(pkg: string, symbols: readonly (ts.Symbol | undefined)[]): ReadonlySet<string> {
    const named = packageNameOf(pkg);
    const home = new Set<string>([named, typesPackageOf(named)]);
    const runtime = RUNTIME_TYPE_PACKAGES.has(named) || RUNTIME_TYPE_PACKAGES.has(typesPackageOf(named));
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
    if (claim.list === 'makes') {
      const base = claim.base;
      if (
        claim.form !== 'call' ||
        claim.member === null ||
        claim.name !== undefined ||
        claim.handler !== undefined ||
        base === undefined ||
        base.arg !== 0 ||
        base.key === undefined
      ) {
        return unchecked('claim_invalid');
      }
      return this.judgeFactory(receiver, claim.member, base.key);
    }
    if (claim.list !== 'ops' || claim.op !== 'request' || claim.handler || claim.ack) {
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

  /** A message claim on its receiver; a maker or scope claim also returns the overloads it holds on. */
  judgeMessage(receiver: MessageReceiver, claim: LibraryClaim): Judged {
    switch (claim.list) {
      case 'makes':
        return this.judgeMakes(receiver, claim);
      case 'scopes':
        return this.judgeScope(receiver, claim);
      case 'ops':
        return { outcome: this.judgeOp(receiver, claim) };
      case 'reserved':
        return { outcome: this.judgeReserved(receiver, claim) };
    }
  }

  /**
   * `member` (or the receiver itself, when null) is callable (`call`) or
   * constructible (`new`) through signatures the package's own declarations
   * write; some overload takes the claimed base, name and handler where the
   * claim puts them, and returns a type that says something. Those overloads
   * are the ones an instance is read through.
   */
  private judgeMakes(receiver: MessageReceiver, claim: MakesClaim): Judged {
    const layout = this.layout({ base: claim.base, name: claim.name, handler: claim.handler });
    if ('failure' in layout) return { outcome: layout.failure };
    const callee = this.ownCallee(receiver, claim.member, claim.form);
    if ('failure' in callee) return { outcome: callee.failure };
    const holding: ts.Signature[] = [];
    let best: RankedFailure | undefined;
    for (const signature of callee.signatures) {
      let result = this.messageSignatureOutcome(signature, layout.value);
      if (result.rank === Infinity && this.returnSaysNothing(this.checker.getReturnTypeOfSignature(signature))) {
        result = { rank: 6, outcome: unchecked('maker_unresolved') };
      }
      if (result.rank === Infinity) holding.push(signature);
      else best = furthest(best, result.rank, result.outcome);
    }
    if (holding.length > 0) return { outcome: VERIFIED, holding };
    return { outcome: best!.outcome };
  }

  /**
   * `member` is the package's own callable member whose name slot accepts a
   * string, under the name-slot rules, and returns a type that says something.
   */
  private judgeScope(receiver: MessageReceiver, claim: ScopesClaim): Judged {
    const layout = this.layout({ name: claim.name });
    if ('failure' in layout) return { outcome: layout.failure };
    const callee = this.ownCallee(receiver, claim.member, 'call');
    if ('failure' in callee) return { outcome: callee.failure };
    const holding: ts.Signature[] = [];
    let best: RankedFailure | undefined;
    for (const signature of callee.signatures) {
      let result = this.messageSignatureOutcome(signature, layout.value);
      if (result.rank === Infinity && this.returnSaysNothing(this.checker.getReturnTypeOfSignature(signature))) {
        result = { rank: 6, outcome: unchecked('scope_unresolved') };
      }
      if (result.rank === Infinity) holding.push(signature);
      else best = furthest(best, result.rank, result.outcome);
    }
    if (holding.length > 0) return { outcome: VERIFIED, holding };
    return { outcome: best!.outcome };
  }

  /**
   * A send, receive or request op: the package's own callable member (or the
   * receiver itself) with some overload that takes the name, payload, handler
   * and acknowledgement where the claim puts them. A name bound by the maker
   * or the scope must have been bound: the instance's maker claim names a
   * `name` slot, or the receiver came through a scope.
   */
  private judgeOp(receiver: MessageReceiver, claim: OpsClaim): Outcome {
    if (claim.method !== undefined || claim.method_key !== undefined || claim.options !== undefined) {
      return unchecked('claim_invalid');
    }
    const name = claim.name;
    // A send carries a payload: a name alone on `send(data)` is the data.
    if (name === undefined || (claim.op === 'send' && claim.payload === undefined)) return unchecked('claim_invalid');
    let nameSlot: ClaimSlot | undefined;
    if ('bound' in name) {
      if (name.bound === 'maker' ? !receiver.makerBindsName : !receiver.scoped) return failed('name_unbound');
    } else {
      nameSlot = name;
    }
    const layout = this.layout({ name: nameSlot, payload: claim.payload, handler: claim.handler, ack: claim.ack });
    if ('failure' in layout) return layout.failure;
    const callee = this.ownCallee(receiver, claim.member, 'call');
    if ('failure' in callee) return callee.failure;
    let best: RankedFailure | undefined;
    for (const signature of callee.signatures) {
      const result = this.messageSignatureOutcome(signature, layout.value);
      if (result.rank === Infinity) return VERIFIED;
      best = furthest(best, result.rank, result.outcome);
    }
    return best!.outcome;
  }

  /**
   * The declarations spell `name` where the member's name slot sits: an
   * overload whose parameter there is that literal, or a key of the map a
   * `keyof` constraint reads. A name the library emits itself only ever
   * removes rows, so this verdict is for the record.
   */
  private judgeReserved(receiver: MessageReceiver, claim: ReservedClaim): Outcome {
    const callee = this.ownCallee(receiver, claim.member, 'call');
    if ('failure' in callee) return callee.failure;
    for (const signature of callee.signatures) {
      if (this.literalsAt(signature, claim.at).has(claim.name)) return VERIFIED;
    }
    return failed('reserved_not_declared');
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
   * 1. A positional name or base accepts a string.
   * 2. A positional name is not a key of an index-signature map; a payload
   *    is there (its type is not read).
   * 3. A positional handler or acknowledgement is a function type with a
   *    declared signature. Keyed parts: the object at that argument declares
   *    every claimed key in one view (one union member), a name or base key
   *    accepting a string and a handler key a declared function.
   * 4. The name is the only place the call takes a string the claim leaves
   *    unassigned: no other positional parameter, and, for a keyed name, no
   *    other key of its object, accepts a string unless the claim assigns it.
   *    Which of two string slots is the name is behaviour, so neither is a
   *    fact (design D2).
   */
  private messageSignatureOutcome(signature: ts.Signature, layout: ClaimLayout): RankedFailure {
    for (const [arg, part] of layout.positional) {
      if (part !== 'name' && part !== 'base') continue;
      const declared = this.keyParameterAt(signature, arg);
      if (declared === undefined) return { rank: 1, outcome: failed('param_missing') };
      const slot = this.throughConditional(declared);
      if (!this.acceptsString(slot)) return { rank: 1, outcome: this.slotFailure(slot, `${part}_not_string`) };
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
      if (part !== 'handler' && part !== 'ack') continue;
      const failure = this.handlerFailure(this.keyParameterAt(signature, arg));
      if (failure) return { rank: 3, outcome: failure };
    }
    const views = new Map<number, ts.Type>();
    for (const [arg, keys] of layout.keyed) {
      const slot = this.keyParameterAt(signature, arg);
      if (slot === undefined) return { rank: 1, outcome: failed('param_missing') };
      const view = this.viewFor(slot, keys);
      if ('rank' in view) return view;
      views.set(arg, view.view);
    }
    const ambiguity = this.nameSiblings(signature, layout, views);
    if (ambiguity) return { rank: 4, outcome: ambiguity };
    return { rank: Infinity, outcome: VERIFIED };
  }

  /**
   * The object at a keyed argument, read as one view: the whole type, or one
   * union member that is an object type and not a function type, declaring
   * every claimed key with the type its part needs.
   */
  private viewFor(slot: Slot, keys: ReadonlyMap<string, PartName>): { view: ts.Type } | RankedFailure {
    if (slot === VARIADIC || !this.isObjectType(slot)) {
      return { rank: 1, outcome: this.slotFailure(slot, 'param_missing') };
    }
    const parts = this.parts(slot);
    const views = parts.length > 1 ? parts.filter(part => this.isPlainObject(part)) : [slot];
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
        if ((part === 'name' || part === 'base') && !this.acceptsString(this.throughConditional(type))) {
          failure = { rank: 3, outcome: this.slotFailure(this.throughConditional(type), 'key_not_string') };
          break;
        }
        if (part === 'payload' && this.isFunctionSlot(type)) {
          failure = { rank: 3, outcome: failed('payload_is_function') };
          break;
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
   * `name_ambiguous` when the call takes a string somewhere the claim leaves
   * unassigned besides the name: another positional parameter (a rest
   * parameter counts once for every position after it, and belongs wholly to
   * the part the claim puts in it), or, for a keyed name, another declared
   * key of the same object. A sibling the claim assigns (payload, handler,
   * base) does not count. No name, no rule.
   */
  private nameSiblings(signature: ts.Signature, layout: ClaimLayout, views: ReadonlyMap<number, ts.Type>): Outcome | undefined {
    const namedAt = [...layout.positional].find(([, part]) => part === 'name')?.[0];
    const keyedName = [...layout.keyed].find(([, keys]) => [...keys.values()].includes('name'));
    if (namedAt === undefined && keyedName === undefined) return undefined;

    const parameters = signature.getParameters();
    const last = parameters[parameters.length - 1];
    const lastDeclaration = last?.valueDeclaration;
    const restIndex =
      lastDeclaration && ts.isParameter(lastDeclaration) && lastDeclaration.dotDotDotToken ? parameters.length - 1 : -1;
    const fixed = restIndex === -1 ? parameters.length : restIndex;
    const ambiguous = failed('name_ambiguous');
    const takesString = (slot: Slot | undefined) => slot !== undefined && slot !== VARIADIC && this.acceptsString(slot);
    // `d2_required_siblings`: a sibling the caller may leave out does not compete.
    const requiredOnly = this.variants.has('d2_required_siblings');
    const isOptional = (parameter: ts.Symbol) => {
      const declaration = parameter.valueDeclaration;
      return (
        declaration !== undefined &&
        ts.isParameter(declaration) &&
        (declaration.questionToken !== undefined || declaration.initializer !== undefined || declaration.dotDotDotToken !== undefined)
      );
    };

    for (let i = 0; i < fixed; i++) {
      if (layout.positional.has(i) || layout.keyed.has(i)) continue;
      if (requiredOnly && isOptional(parameters[i])) continue;
      if (takesString(this.keyParameterAt(signature, i))) return ambiguous;
    }
    if (restIndex !== -1) {
      const rest = this.checker.getTypeOfSymbol(last);
      if (this.checker.isTupleType(rest)) {
        const elements = this.checker.getTypeArguments(rest as ts.TypeReference);
        const flags = ((rest as ts.TypeReference).target as ts.TupleType).elementFlags;
        // From the first variable element on (`[...channels: string[], cb]`),
        // no position is fixed: a name there is one of many.
        const variable = flags.findIndex(flag => (flag & ts.ElementFlags.Variable) !== 0);
        const fixedLength = variable === -1 ? elements.length : variable;
        for (let i = restIndex; i < restIndex + fixedLength; i++) {
          if (layout.positional.has(i) || layout.keyed.has(i)) continue;
          if (requiredOnly && (flags[i - restIndex] & ts.ElementFlags.Optional) !== 0) continue;
          if (takesString(this.keyParameterAt(signature, i))) return ambiguous;
        }
        if (variable !== -1) {
          const unfixed = [...layout.positional].filter(([arg]) => arg >= restIndex + variable).map(([, part]) => part);
          if (unfixed.includes('name')) return ambiguous;
          const element = elements[variable];
          const elementTakesString =
            element !== undefined &&
            (this.checker.isArrayType(element)
              ? takesString(this.checker.getTypeArguments(element as ts.TypeReference)[0])
              : takesString(element));
          if (unfixed.length === 0 && elementTakesString && !requiredOnly) return ambiguous;
        }
      } else {
        const inRest = [...layout.positional].filter(([arg]) => arg >= restIndex).map(([, part]) => part);
        // A rest the claim puts a payload or handler in is that part's; one
        // that holds the name holds other names too.
        const ownedByOtherPart = inRest.length > 0 && !inRest.includes('name');
        // A rest may be empty, so under `d2_required_siblings` only a rest that holds the name competes.
        const competes = inRest.includes('name') || (!ownedByOtherPart && !requiredOnly);
        if (competes && takesString(this.keyParameterAt(signature, restIndex))) return ambiguous;
      }
    }
    if (keyedName) {
      const [arg, keys] = keyedName;
      const view = views.get(arg);
      if (view) {
        for (const property of this.declaredProperties(view)) {
          if (keys.has(property.getName())) continue;
          if (requiredOnly && property.flags & ts.SymbolFlags.Optional) continue;
          if (this.acceptsString(this.checker.getTypeOfSymbol(property))) return ambiguous;
        }
      }
    }
    return undefined;
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
   * The map a `keyof` reads has an index signature. Under
   * `index_key_generic_map`, a map the receiver takes as a type parameter is
   * the caller's to type, so its key slot reads as a string slot.
   */
  private isIndexMap(map: ts.Type): boolean {
    if (this.variants.has('index_key_generic_map') && map.flags & ts.TypeFlags.TypeParameter) return false;
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
    const slot = this.throughConstraint(declared);
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
   * reads, both as instantiated and as written at the declaration.
   */
  private literalsAt(signature: ts.Signature, at: ClaimSlot): ReadonlySet<string> {
    const literals = new Set<string>();
    let slot: Slot | undefined = this.parameterAt(signature, at.arg);
    if (slot !== undefined && slot !== VARIADIC && at.key !== undefined) {
      const property = this.declaredProperty(slot, at.key);
      slot = property ? this.checker.getTypeOfSymbol(property) : undefined;
    }
    if (slot !== undefined && slot !== VARIADIC) this.spell(slot, literals, 0);
    if (at.key === undefined) {
      const parameters = signature.getDeclaration()?.parameters;
      const node = parameters?.[Math.min(at.arg, parameters.length - 1)]?.type;
      if (node) this.spell(this.checker.getTypeFromTypeNode(node), literals, 0);
    }
    return literals;
  }

  private spell(type: ts.Type, into: Set<string>, depth: number): void {
    if (depth > 6) return;
    if (type.isStringLiteral()) {
      into.add(type.value);
      return;
    }
    if (type.isUnionOrIntersection()) {
      for (const part of type.types) this.spell(part, into, depth + 1);
      return;
    }
    if (type.flags & ts.TypeFlags.TypeParameter) {
      const constraintNode = this.declaredConstraint(type);
      this.spell(
        constraintNode ? this.checker.getTypeFromTypeNode(constraintNode) : this.checker.getBaseConstraintOfType(type) ?? type,
        into,
        depth + 1
      );
      return;
    }
    if (type.flags & ts.TypeFlags.Index) {
      let map = (type as ts.IndexType).type;
      if (map.flags & ts.TypeFlags.TypeParameter) {
        map = this.checker.getDefaultFromTypeParameter(map) ?? this.checker.getBaseConstraintOfType(map) ?? map;
      }
      for (const property of this.checker.getPropertiesOfType(this.checker.getApparentType(map))) {
        if (!property.getName().startsWith('__@')) into.add(property.getName());
      }
    }
  }

  /**
   * The callee of a message claim: `member` of the receiver, or the receiver
   * itself when null, called (`call`) or constructed (`new`), through the
   * signatures the receiver's own packages declare. A member, or every
   * signature, that only another package declares (the runtime's event
   * emitter a library class extends, a base class from a dependency) is
   * `member_inherited`.
   */
  private ownCallee(receiver: MessageReceiver, member: string | null, form: 'call' | 'new'): Callable {
    let type = receiver.type;
    // Under `inherited_bound_emitter`, a member of a base the receiver binds with its own type.
    let boundBase = false;
    if (member !== null) {
      const apparent = this.checker.getApparentType(this.checker.getNonNullableType(receiver.type));
      const property = this.declaredProperty(receiver.type, member);
      if (!property) return { failure: failed('member_missing') };
      if (!this.isOwnMember(property, apparent, receiver.home)) {
        if (!(this.variants.has('inherited_bound_emitter') && this.isBoundByOwnType(property, receiver))) {
          return { failure: failed('member_inherited') };
        }
        boundBase = true;
      }
      type = this.checker.getTypeOfSymbol(property);
    }
    const all =
      form === 'new'
        ? this.checker.getNonNullableType(type).getConstructSignatures()
        : this.checker.getNonNullableType(type).getCallSignatures();
    const library = all.filter(signature => {
      const file = this.signatureFile(signature, type, form);
      return file !== undefined && this.isLibraryFile(file);
    });
    const own = boundBase
      ? library
      : library.filter(signature => this.isOwnFile(this.signatureFile(signature, type, form)!, receiver.home));
    if (own.length > 0) return { signatures: own };
    if (library.length > 0) return { failure: failed('member_inherited') };
    return { failure: this.slotFailure(type, form === 'new' ? 'member_not_constructible' : 'member_not_callable') };
  }

  /**
   * `inherited_bound_emitter`: the member is declared on a base type another
   * package writes, and the receiver's class binds that base with at least
   * one concrete type argument (not a type parameter) its own packages
   * declare: `class Socket<L, E> extends Emitter<L, E, SocketReservedEvents>`.
   * The runtime's emitter, extended with no type of the package's own, is
   * not bound; nor is a base bound only through the class's type parameters.
   */
  private isBoundByOwnType(property: ts.Symbol, receiver: MessageReceiver): boolean {
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
        (symbol?.declarations ?? []).some(declaration => this.isOwnFile(declaration.getSourceFile(), receiver.home))
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
    return visit(this.checker.getNonNullableType(receiver.type), 0);
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
  // Surface listing
  // --------------------------------------------------------------------------

  /** One package's declared surface (see `LibraryClaimsVerifier.listSurface`). */
  listPackage(pkg: string, declaration: ts.ImportDeclaration, maxEntries: number): LibrarySurface {
    this.setPackage(pkg);
    const moduleRead = this.readModule(pkg, declaration);
    const surface: LibrarySurface = { package: pkg, truncated: 0, exports: [] };
    if (moduleRead.entry.resolved_file) surface.resolved_file = moduleRead.entry.resolved_file;
    if (moduleRead.entry.installed_version) surface.installed_version = moduleRead.entry.installed_version;
    if (moduleRead.reason) return { ...surface, reason: moduleRead.reason };
    const moduleSymbol = this.checker.getSymbolAtLocation(declaration.moduleSpecifier);
    if (!moduleSymbol) return { ...surface, reason: 'module_unresolved' };

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

    const exportsOf = this.checker
      .getExportsOfModule(moduleSymbol)
      .filter(symbol => {
        const target = symbol.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(symbol) : symbol;
        return !this.checker.isUnknownSymbol(target) && (target.flags & ts.SymbolFlags.Value) !== 0;
      })
      .sort((a, b) => (a.getName() < b.getName() ? -1 : a.getName() > b.getName() ? 1 : 0));

    for (const symbol of exportsOf) {
      if (!take()) continue;
      const target = symbol.flags & ts.SymbolFlags.Alias ? this.checker.getAliasedSymbol(symbol) : symbol;
      const type = this.checker.getTypeOfSymbolAtLocation(symbol, declaration);
      const name = symbol.getName();
      const entry: SurfaceExport = { export: name === 'default' ? 'default' : name, receivers: [] };
      if (this.isOpenTop(type)) {
        surface.exports.push(entry);
        continue;
      }
      const receiverOf = (receiverName: string, receiverType: ts.Type): SurfaceReceiver => {
        const home = this.homeOf(pkg, [target, ...typeSymbols(receiverType)]);
        return this.listReceiver(receiverName, receiverType, home, take);
      };
      entry.receivers.push(receiverOf('export', type));
      const made = (signatures: readonly ts.Signature[], receiverName: string) => {
        const returns = [...new Set(signatures.map(sig => this.checker.getReturnTypeOfSignature(sig)))];
        if (returns.length === 1 && !this.returnSaysNothing(returns[0]) && this.isObjectType(returns[0])) {
          entry.receivers.push(receiverOf(receiverName, returns[0]));
        }
      };
      made(this.librarySignatures(type, 'call'), 'instance:()');
      made(this.librarySignatures(type, 'new'), 'instance:new');
      surface.exports.push(entry);
    }
    surface.truncated = dropped;
    surface.sha256 = crypto.createHash('sha256').update(JSON.stringify(surface.exports)).digest('hex');
    return surface;
  }

  private librarySignatures(type: ts.Type, form: 'call' | 'new'): readonly ts.Signature[] {
    const nonNullable = this.checker.getNonNullableType(type);
    const all = form === 'new' ? nonNullable.getConstructSignatures() : nonNullable.getCallSignatures();
    return all.filter(signature => {
      const file = this.signatureFile(signature, nonNullable, form);
      return file !== undefined && this.isLibraryFile(file);
    });
  }

  private listReceiver(
    receiverName: string,
    type: ts.Type,
    home: ReadonlySet<string>,
    take: () => boolean
  ): SurfaceReceiver {
    const receiver: SurfaceReceiver = { receiver: receiverName, members: [] };
    const call = this.librarySignatures(type, 'call');
    if (call.length > 0) receiver.call = this.listSignatures(call, take);
    const construct = this.librarySignatures(type, 'new');
    if (construct.length > 0) receiver.construct = this.listSignatures(construct, take);
    if (this.isOpenTop(type)) return receiver;
    const apparent = this.checker.getApparentType(this.checker.getNonNullableType(type));
    for (const property of this.declaredProperties(type)) {
      const signatures = this.librarySignatures(this.checker.getTypeOfSymbol(property), 'call');
      if (signatures.length === 0 || !take()) continue;
      const member: SurfaceMember = {
        name: property.getName(),
        own: this.isOwnMember(property, apparent, home),
        signatures: this.listSignatures(signatures, take),
      };
      receiver.members.push(member);
    }
    return receiver;
  }

  private listSignatures(signatures: readonly ts.Signature[], take: () => boolean): SurfaceSignature[] {
    const listed: SurfaceSignature[] = [];
    for (const signature of signatures) {
      if (!take()) continue;
      const params: SurfaceParam[] = [];
      for (const parameter of signature.getParameters()) {
        if (!take()) continue;
        const declaration = parameter.valueDeclaration;
        const isParameter = declaration !== undefined && ts.isParameter(declaration);
        const type = this.checker.getTypeOfSymbol(parameter);
        const slot = this.throughConstraint(type);
        const param: SurfaceParam = {
          name: parameter.getName(),
          optional: isParameter && (declaration.questionToken !== undefined || declaration.initializer !== undefined),
          rest: isParameter && declaration.dotDotDotToken !== undefined,
          type: truncate(this.checker.typeToString(type)),
          accepts_string: this.acceptsString(type),
          function: this.handlerFailure(type) === undefined,
        };
        if (slot !== VARIADIC && this.isObjectType(slot) && this.handlerFailure(slot) !== undefined) {
          const keys: SurfaceKey[] = [];
          for (const property of this.declaredProperties(slot)) {
            if (!take()) continue;
            const keyType = this.checker.getTypeOfSymbol(property);
            keys.push({
              name: property.getName(),
              accepts_string: this.acceptsString(keyType),
              function: this.handlerFailure(keyType) === undefined,
            });
          }
          if (keys.length > 0) param.keys = keys;
        }
        const literals = new Set<string>();
        this.spell(type, literals, 0);
        const node = isParameter ? declaration.type : undefined;
        if (node) this.spell(this.checker.getTypeFromTypeNode(node), literals, 0);
        if (literals.size > 0) param.literals = [...literals].sort();
        params.push(param);
      }
      listed.push({ params, returns: truncate(this.checker.typeToString(this.checker.getReturnTypeOfSignature(signature))) });
    }
    return listed;
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
    if (signatures.length > 0) return { signatures };
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
    return this.declaredProperties(slot).find(property => property.getName() === name);
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

  private declaredProperties(slot: Slot): ts.Symbol[] {
    if (slot === VARIADIC) return [];
    const apparent = this.checker.getApparentType(this.checker.getNonNullableType(slot));
    return this.checker
      .getPropertiesOfType(apparent)
      .filter(
        property =>
          !this.isBuiltinMember(property) &&
          this.isLibraryDeclared(property, apparent) &&
          !this.isAbsent(property)
      );
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
    this.currentPackage = packageNameOf(pkg);
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
   * a pnpm store (`node_modules/.pnpm/pkg@1/node_modules/pkg`) are.
   */
  private installedPackage(fileName: string): InstalledPackage | undefined {
    const segments = path.relative(this.root, this.realpath(fileName)).split(path.sep);
    const last = segments.lastIndexOf('node_modules');
    if (last < 0) return undefined;
    const width = segments[last + 1]?.startsWith('@') ? 2 : 1;
    const nameSegments = segments.slice(last + 1, last + 1 + width);
    // No package.json there (a file directly under node_modules included) means no package.
    const directory = path.resolve(this.root, ...segments.slice(0, last + 1 + width));
    let known = this.packageDirectories.get(directory);
    if (known === undefined) {
      known = readInstalledPackage(this.host, directory, nameSegments.join('/'));
      this.packageDirectories.set(directory, known);
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
   * conditional return type, does.
   */
  returnSaysNothing(type: ts.Type): boolean {
    if (this.saysNothing(type)) return true;
    return this.parts(type).some(part => {
      if (!(part.flags & ts.TypeFlags.Conditional)) return false;
      const node = (part as ts.ConditionalType).root.node;
      return [node.trueType, node.falseType].some(branch =>
        this.returnSaysNothing(this.checker.getTypeFromTypeNode(branch))
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

/** A package directory under `node_modules`: the name it is installed as, and the name its package.json gives. */
interface InstalledPackage {
  directoryName: string;
  packageName: string | undefined;
}

function readInstalledPackage(
  host: ts.ModuleResolutionHost,
  directory: string,
  directoryName: string
): InstalledPackage | null {
  const manifest = path.join(directory, 'package.json');
  if (!host.fileExists(manifest)) return null;
  let packageName: string | undefined;
  try {
    const parsed: unknown = JSON.parse(host.readFile(manifest) ?? '');
    const name = (parsed as { name?: unknown } | null)?.name;
    if (typeof name === 'string') packageName = name;
  } catch {
    // An unreadable manifest still marks an installed directory; it names no package.
  }
  return { directoryName, packageName };
}

function isNullish(type: ts.Type): boolean {
  return (type.flags & (ts.TypeFlags.Null | ts.TypeFlags.Undefined | ts.TypeFlags.Void)) !== 0;
}

/** `string`, a string literal or template, or an intersection with one (`string & {}`). */
function isStringLike(type: ts.Type): boolean {
  if (type.flags & ts.TypeFlags.StringLike) return true;
  return type.isIntersection() && type.types.some(isStringLike);
}
