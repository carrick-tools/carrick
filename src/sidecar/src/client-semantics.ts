/**
 * `verify_client_semantics` (carrick#1564): check what a model said about an
 * HTTP client library against the package's own type declarations.
 *
 * A claim names a member of a package export (or of an instance one of its
 * factories returns) and says how that member is called: which option key
 * carries the base URL, which HTTP method a verb sends, where the path, method
 * and body sit. The scanner reads call sites through a claim only when this
 * check verified it, and a verified claim becomes a fact that can fail a pull
 * request check. So a claim is `verified` only when the declarations say so
 * positively. `failed` means the declarations resolved and contradict the
 * claim; `unchecked` means they could not be read, or say nothing at the place
 * the claim needs (`any`, `unknown`, `{}`, an unconstrained type parameter).
 * The scanner drops both.
 *
 * The export's type is read the way the service's own code would read it: a
 * probe file in `from_dir` imports it, inside the service's program, under the
 * service's compiler options. The declarations must be the installed
 * package's: a `paths` alias or a `declare module` block the service writes
 * for itself is not evidence about the library.
 */

import * as path from 'node:path';
import { ts, type Project } from 'ts-morph';
import type {
  SemanticsCheck,
  SemanticsModule,
  SemanticsResult,
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

/** A resolved module lands on TypeScript: a declaration file or source. */
const TYPESCRIPT_FILE = /\.(d\.[mc]?ts|[mc]?tsx?)$/;

const IDENTIFIER = /^[A-Za-z_$][\w$]*$/;

const RECEIVER = /^(export|instance:\S+)$/;

type Claim = SemanticsCheck['claim'];
type RequestArgs = 'config' | 'path_options';

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

export interface VerifyResult {
  semantics: SemanticsResult[];
  modules: SemanticsModule[];
}

let probeSequence = 0;

export class ClientSemanticsVerifier {
  constructor(private readonly project: Project) {}

  /**
   * Judge every check, in request order, spending at most `budgetMs`. Checks
   * the budget does not reach come back `unchecked` with reason `budget`.
   */
  run(fromDir: string, checks: SemanticsCheck[], budgetMs: number): VerifyResult {
    const deadline = performance.now() + budgetMs;
    const stamp = (check: SemanticsCheck, outcome: Outcome): SemanticsResult =>
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
    // The base-URL keys the request's factory claims name, per factory: an
    // instance is read through the overload that declares them.
    const factoryKeys = new Map<string, Set<string>>();
    for (const check of checks) {
      const key = exportKey(check);
      if (!importIndex.has(key)) {
        importIndex.set(key, importKeys.length);
        importKeys.push(key);
      }
      if (check.claim.kind === 'factory') {
        const factory = factoryKey(key, check.claim.member);
        const keys = factoryKeys.get(factory) ?? new Set<string>();
        keys.add(check.claim.base_url_key);
        factoryKeys.set(factory, keys);
      }
    }
    const probeText = importKeys
      .map((key, i) => {
        const [pkg, name] = JSON.parse(key) as [string, string];
        return importLine(pkg, name, `__carrick_e${i}`);
      })
      .join('\n');

    const probePath = path.join(
      fromDir,
      `__carrick_semantics_probe_${process.pid}_${probeSequence++}.ts`
    );
    const probe = this.project.createSourceFile(probePath, `${probeText}\n`, { overwrite: true });
    try {
      const program = this.project.getProgram().compilerObject;
      const file = program.getSourceFile(probe.getFilePath());
      if (!file) throw new Error(`probe file ${probePath} is not in the program`);
      const reader = new DeclarationReader(
        program,
        file,
        this.project.getModuleResolutionHost()
      );

      const moduleReads = new Map<string, ModuleRead>();
      const exportReads = new Map<string, Read<ts.Type>>();
      const receiverReads = new Map<string, Read<ts.Type>>();
      const semantics: SemanticsResult[] = [];

      for (const check of checks) {
        if (performance.now() >= deadline) {
          semantics.push(stamp(check, unchecked('budget')));
          continue;
        }
        if (!RECEIVER.test(check.receiver)) {
          semantics.push(stamp(check, unchecked('receiver_invalid')));
          continue;
        }
        const key = exportKey(check);
        const declaration = file.statements[importIndex.get(key)!] as ts.ImportDeclaration;

        let moduleRead = moduleReads.get(check.package);
        if (!moduleRead) {
          moduleRead = reader.readModule(check.package, declaration);
          moduleReads.set(check.package, moduleRead);
        }
        if (moduleRead.reason) {
          semantics.push(stamp(check, unchecked(moduleRead.reason)));
          continue;
        }

        let exportRead = exportReads.get(key);
        if (!exportRead) {
          exportRead = reader.readExport(declaration);
          exportReads.set(key, exportRead);
        }
        if ('reason' in exportRead) {
          semantics.push(stamp(check, unchecked(exportRead.reason)));
          continue;
        }

        const receiverKey = `${key}\u0000${check.receiver}`;
        let receiverRead = receiverReads.get(receiverKey);
        if (!receiverRead) {
          const factory = check.receiver.startsWith('instance:')
            ? check.receiver.slice('instance:'.length)
            : undefined;
          receiverRead = reader.readReceiver(
            exportRead.value,
            factory,
            factory === undefined ? undefined : factoryKeys.get(factoryKey(key, factory))
          );
          receiverReads.set(receiverKey, receiverRead);
        }
        if ('reason' in receiverRead) {
          semantics.push(stamp(check, unchecked(receiverRead.reason)));
          continue;
        }

        semantics.push(stamp(check, reader.judge(receiverRead.value, check.claim)));
      }

      return {
        semantics,
        modules: [...moduleReads.values()].map(read => read.entry),
      };
    } finally {
      this.project.removeSourceFile(probe);
    }
  }
}

function exportKey(check: SemanticsCheck): string {
  return JSON.stringify([check.package, check.export]);
}

function factoryKey(exportKeyText: string, member: string): string {
  return `${exportKeyText}\u0000${member}`;
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
 * Reads the declarations the probe imported, with the program's own checker.
 * Every predicate is the contract's (carrick#1564, section 3), stated on the
 * checker's public API, with the definitions the review amended.
 */
class DeclarationReader {
  private readonly checker: ts.TypeChecker;

  constructor(
    private readonly program: ts.Program,
    private readonly probe: ts.SourceFile,
    private readonly host: ts.ModuleResolutionHost
  ) {
    this.checker = program.getTypeChecker();
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
      const agrees =
        resolved !== undefined && this.program.getSourceFile(resolved.resolvedFileName) === file;
      if (agrees) {
        const version = resolved.packageId?.version;
        if (version) entry.installed_version = version;
      }
      if (!TYPESCRIPT_FILE.test(file.fileName)) return done('module_js_only');
      if (!agrees || !resolved.isExternalLibraryImport) return done('module_local');
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

  /** `T`: the type the probe's import gets. */
  readExport(declaration: ts.ImportDeclaration): Read<ts.Type> {
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
    return { value: type };
  }

  /**
   * `R`: the export itself, or what its factory `factory` returns. The
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

  // --------------------------------------------------------------------------
  // Claims
  // --------------------------------------------------------------------------

  judge(receiver: ts.Type, claim: Claim): Outcome {
    switch (claim.kind) {
      case 'factory':
        return this.judgeFactory(receiver, claim.member, claim.base_url_key);
      case 'verb':
        return this.judgeVerb(receiver, claim.member, claim.method);
      case 'verb_body':
        return this.judgeVerbBody(receiver, claim.member, claim.args, claim.body_key);
      case 'request': {
        const selected = this.selectRequest(receiver, claim.member, claim.args, claim.url_key, claim.method_key);
        return 'failure' in selected ? selected.failure : VERIFIED;
      }
      case 'request_body':
        return this.judgeRequestBody(receiver, claim);
    }
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
    const first = this.parameterAt(signature, 0);
    if (first === undefined) return { rank: 1, outcome: failed('param_missing') };
    const key = this.declaredProperty(first, baseUrlKey);
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
      const first = this.parameterAt(signature, 0);
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
      const first = this.parameterAt(signature, 0);
      const second = this.parameterAt(signature, 1);
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
   * The signatures the `request` claim with the same keys selects, and
   * `body_key` declared on the config of one of them: the first parameter for
   * `config`, the second for `path_options`.
   */
  private judgeRequestBody(receiver: ts.Type, claim: Extract<Claim, { kind: 'request_body' }>): Outcome {
    const selected = this.selectRequest(receiver, claim.member, claim.args, claim.url_key, claim.method_key);
    if ('failure' in selected) return selected.failure;
    const position = claim.args === 'config' ? 0 : 1;
    const declared = selected.signatures.some(signature => {
      const config = this.parameterAt(signature, position);
      return config !== undefined && this.declaredProperty(config, claim.body_key) !== undefined;
    });
    return declared ? VERIFIED : failed('key_missing');
  }

  /**
   * The callee's signatures a request claim can be read through. The callee
   * is `receiver[member]`, or the receiver itself when `member` is null.
   *
   * `config`: the first parameter is an object type declaring `url_key`
   * accepting string and `method_key` accepting string or an HTTP method
   * literal. `path_options`: the first parameter accepts string and the second
   * is an object type declaring `method_key` accepting the same.
   */
  private selectRequest(
    receiver: ts.Type,
    member: string | null,
    args: RequestArgs,
    urlKey: string | undefined,
    methodKey: string
  ): { signatures: readonly ts.Signature[] } | { failure: Outcome } {
    const callable =
      member === null ? this.callSignatures(receiver) : this.callableProperty(receiver, member);
    if ('failure' in callable) return callable;

    const selected: ts.Signature[] = [];
    let best: RankedFailure | undefined;
    for (const signature of callable.signatures) {
      const first = this.parameterAt(signature, 0);
      let config: Slot | undefined = first;
      if (args === 'path_options') {
        if (first === undefined || !this.acceptsString(first)) {
          best = furthest(best, 1, first === undefined ? failed('param_missing') : this.slotFailure(first, 'param_missing'));
          continue;
        }
        config = this.parameterAt(signature, 1);
      }
      if (config === undefined) {
        best = furthest(best, 1, failed('param_missing'));
        continue;
      }
      if (!this.isObjectType(config)) {
        best = furthest(best, 1, this.slotFailure(config, 'param_missing'));
        continue;
      }
      // A `config` claim names its url key; with none there is nothing to find.
      const url =
        args === 'config'
          ? urlKey === undefined
            ? undefined
            : this.declaredProperty(config, urlKey)
          : null;
      const method = this.declaredProperty(config, methodKey);
      if (url === undefined || !method) {
        best = furthest(best, 2, failed('key_missing'));
        continue;
      }
      const urlType = url === null ? undefined : this.checker.getTypeOfSymbol(url);
      if (urlType !== undefined && !this.acceptsString(urlType)) {
        best = furthest(best, 3, this.slotFailure(urlType, 'key_not_string'));
        continue;
      }
      const methodType = this.checker.getTypeOfSymbol(method);
      if (!this.acceptsMethod(methodType)) {
        best = furthest(best, 3, this.slotFailure(methodType, 'key_not_string'));
        continue;
      }
      selected.push(signature);
    }
    if (selected.length > 0) return { signatures: selected };
    return { failure: best!.outcome };
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

  private callSignatures(type: ts.Type): Callable {
    const signatures = this.checker.getNonNullableType(type).getCallSignatures();
    if (signatures.length > 0) return { signatures };
    return { failure: this.slotFailure(type, 'member_not_callable') };
  }

  /**
   * A declared property of `type`: one the checker lists on its apparent type
   * with null and undefined removed, and not one every value of that kind
   * inherits (`constructor`, `toString`, a primitive wrapper's members). An
   * index signature does not count.
   */
  private declaredProperty(slot: Slot, name: string): ts.Symbol | undefined {
    return this.declaredProperties(slot).find(property => property.getName() === name);
  }

  private declaredProperties(slot: Slot): ts.Symbol[] {
    if (slot === VARIADIC) return [];
    const apparent = this.checker.getApparentType(this.checker.getNonNullableType(slot));
    return this.checker.getPropertiesOfType(apparent).filter(property => !this.isBuiltinMember(property));
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
  private acceptsString(slot: Slot): boolean {
    if (slot === VARIADIC) return false;
    return (
      this.checker.isTypeAssignableTo(this.checker.getStringType(), slot) &&
      this.parts(slot).some(part => isStringLike(part))
    );
  }

  /** Accepts string, or names an HTTP method as a literal in either case. */
  private acceptsMethod(slot: Slot): boolean {
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
  private returnSaysNothing(type: ts.Type): boolean {
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
   * parameter; `undefined` when it has none there.
   */
  private parameterAt(signature: ts.Signature, index: number): Slot | undefined {
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
    const rest = this.checker.getTypeOfSymbol(last);
    if (this.checker.isTupleType(rest)) {
      return this.checker.getTypeArguments(rest as ts.TypeReference)[index - restIndex];
    }
    if (this.checker.isArrayType(rest)) {
      return this.checker.getTypeArguments(rest as ts.TypeReference)[0];
    }
    return this.isOpenTop(rest) ? rest : VARIADIC;
  }
}

function isNullish(type: ts.Type): boolean {
  return (type.flags & (ts.TypeFlags.Null | ts.TypeFlags.Undefined | ts.TypeFlags.Void)) !== 0;
}

/** `string`, a string literal or template, or an intersection with one (`string & {}`). */
function isStringLike(type: ts.Type): boolean {
  if (type.flags & ts.TypeFlags.StringLike) return true;
  return type.isIntersection() && type.types.some(isStringLike);
}
