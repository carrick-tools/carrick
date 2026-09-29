/**
 * `verify_client_semantics` (carrick#1564): check what a model said about an
 * HTTP client library against the package's own type declarations.
 *
 * A claim names a member of a package export (or of an instance one of its
 * factories returns) and says how that member is called: which option key
 * carries the base URL, which HTTP method a verb sends, where the path, method
 * and body sit. The scanner reads call sites through a claim only when this
 * check verified it, so a claim is `verified` only when the declarations say
 * so. `failed` means the declarations resolved and contradict the claim;
 * `unchecked` means they could not be read. The scanner drops both.
 *
 * The export's type is read the way the service's own code would read it: a
 * probe file in `from_dir` imports it, inside the service's program, under the
 * service's compiler options. An export typed `any` or `unknown` verifies
 * nothing, because an untyped module (or a shorthand `declare module "x";`)
 * would otherwise satisfy every claim.
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

/** A resolved module lands on TypeScript: a declaration file or source. */
const TYPESCRIPT_FILE = /\.(d\.[mc]?ts|[mc]?tsx?)$/;

const IDENTIFIER = /^[A-Za-z_$][\w$]*$/;

type Claim = SemanticsCheck['claim'];

/** How one check came out, before it is stamped with its claim and receiver. */
type Outcome =
  | { verdict: 'verified' }
  | { verdict: 'failed' | 'unchecked'; reason: string };

const VERIFIED: Outcome = { verdict: 'verified' };
const failed = (reason: string): Outcome => ({ verdict: 'failed', reason });
const unchecked = (reason: string): Outcome => ({ verdict: 'unchecked', reason });

/**
 * A failure some signature reached, ranked by how far into the predicate it
 * got. When no signature satisfies a claim, the verdict reports the one that
 * got furthest, so a wrong key reads `key_missing` rather than the
 * `param_missing` of an unrelated overload.
 */
interface RankedFailure {
  rank: number;
  outcome: Outcome;
}

function furthest(current: RankedFailure | undefined, rank: number, outcome: Outcome): RankedFailure {
  return current && current.rank >= rank ? current : { rank, outcome };
}

/** A read that either produced a value or a reason it could not. */
type Read<T> = { value: T } | { reason: string };

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
    for (const check of checks) {
      const key = exportKey(check);
      if (!importIndex.has(key)) {
        importIndex.set(key, importKeys.length);
        importKeys.push(key);
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
          receiverRead = reader.readReceiver(exportRead.value, check.receiver);
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

/** The probe's import of one export, bound to `local`. */
function importLine(pkg: string, name: string, local: string): string {
  const specifier = JSON.stringify(pkg);
  if (name === 'default') return `import ${local} from ${specifier};`;
  const imported = IDENTIFIER.test(name) ? name : JSON.stringify(name);
  return `import { ${imported} as ${local} } from ${specifier};`;
}

/**
 * Reads the declarations the probe imported, with the program's own checker.
 * Every predicate is the contract's (carrick#1564, section 3), stated on the
 * checker's public API.
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
   * Where the package resolved. The checker's module symbol is the answer: it
   * sees what the service's program sees, ambient `declare module` blocks
   * included. The resolver's own answer adds the installed version, and tells
   * a JS-only package from one that is not there when the checker has no
   * symbol at all.
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
    const version = resolved?.packageId?.version;
    if (version) entry.installed_version = version;

    const moduleSymbol = this.checker.getSymbolAtLocation(specifier);
    const declared = moduleSymbol?.declarations?.[0]?.getSourceFile().fileName;
    const resolvedFile = declared ?? resolved?.resolvedFileName;
    if (resolvedFile) entry.resolved_file = resolvedFile;

    if (!resolvedFile) return { entry: { ...entry, reason: 'module_unresolved' }, reason: 'module_unresolved' };
    if (!TYPESCRIPT_FILE.test(resolvedFile)) {
      return { entry: { ...entry, reason: 'module_js_only' }, reason: 'module_js_only' };
    }
    return { entry };
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
    if (isOpenTop(type)) return { reason: 'export_untyped' };
    return { value: type };
  }

  /** `R`: the export itself, or what one of its factories returns. */
  readReceiver(exported: ts.Type, receiver: string): Read<ts.Type> {
    if (receiver === 'export') return { value: exported };
    const factory = receiver.slice('instance:'.length);
    const callable = this.callableProperty(exported, factory);
    if ('reason' in callable) return { reason: 'factory_unresolved' };
    const signatures = callable.value;
    const signature =
      signatures.find(sig => {
        const first = this.parameterType(sig, 0);
        return first !== undefined && this.isObjectType(first);
      }) ?? (signatures.length === 1 ? signatures[0] : undefined);
    if (!signature) return { reason: 'factory_unresolved' };
    const instance = this.checker.getReturnTypeOfSignature(signature);
    if (isOpenTop(instance)) return { reason: 'factory_unresolved' };
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
        const selected = this.selectRequest(receiver, claim.member, claim.args, {
          urlKey: claim.url_key,
          methodKey: claim.method_key,
        });
        return 'failure' in selected ? selected.failure : VERIFIED;
      }
      case 'request_body':
        return this.judgeRequestBody(receiver, claim.member, claim.args, claim.body_key);
    }
  }

  /**
   * `member` is a declared callable property of the receiver; some signature's
   * first parameter declares `base_url_key` accepting string, and returns a
   * type that is not `any` or `unknown`.
   */
  private judgeFactory(receiver: ts.Type, member: string, baseUrlKey: string): Outcome {
    const callable = this.callableProperty(receiver, member);
    if ('reason' in callable) return failed(callable.reason);
    let best: RankedFailure | undefined;
    for (const signature of callable.value) {
      const first = this.parameterType(signature, 0);
      if (!first) {
        best = furthest(best, 1, failed('param_missing'));
        continue;
      }
      const key = this.declaredProperty(first, baseUrlKey);
      if (!key) {
        best = furthest(best, 2, failed('key_missing'));
        continue;
      }
      if (!this.acceptsString(this.checker.getTypeOfSymbol(key))) {
        best = furthest(best, 3, failed('key_not_string'));
        continue;
      }
      // The option is there, but what the factory builds cannot be read, so
      // nothing it returns can be checked either.
      if (isOpenTop(this.checker.getReturnTypeOfSignature(signature))) {
        best = furthest(best, 4, unchecked('factory_unresolved'));
        continue;
      }
      return VERIFIED;
    }
    return best!.outcome;
  }

  /**
   * `method` is `member` upper-cased and an HTTP method (a method is not a
   * type-level fact, so this is read off the claim itself); `member` is a
   * declared callable property whose first parameter accepts string.
   */
  private judgeVerb(receiver: ts.Type, member: string, method: string): Outcome {
    if (method !== member.toUpperCase() || !HTTP_METHODS.has(method)) {
      return failed('method_not_member_verb');
    }
    const callable = this.callableProperty(receiver, member);
    if ('reason' in callable) return failed(callable.reason);
    const takesPath = callable.value.some(signature => {
      const first = this.parameterType(signature, 0);
      return first !== undefined && this.acceptsString(first);
    });
    return takesPath ? VERIFIED : failed('path_not_string');
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
    if ('reason' in callable) return failed(callable.reason);
    let best: RankedFailure | undefined;
    for (const signature of callable.value) {
      const first = this.parameterType(signature, 0);
      const second = this.parameterType(signature, 1);
      if (!first || !this.acceptsString(first) || !second) {
        best = furthest(best, 1, failed('param_missing'));
        continue;
      }
      if (args === 'path_body') {
        if (!isOpen(second)) {
          best = furthest(best, 2, failed('body_not_open'));
          continue;
        }
        return VERIFIED;
      }
      if (!this.isObjectType(second) || this.declaredProperties(second).length === 0) {
        best = furthest(best, 2, failed('options_not_object'));
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
   * The request signatures `request` selects with the same `args`, and
   * `body_key` declared on the config of one of them: the first parameter for
   * `config`, the second for `path_options`.
   */
  private judgeRequestBody(
    receiver: ts.Type,
    member: string | null,
    args: 'config' | 'path_options',
    bodyKey: string
  ): Outcome {
    const selected = this.selectRequest(receiver, member, args, {});
    if ('failure' in selected) return selected.failure;
    const position = args === 'config' ? 0 : 1;
    const declared = selected.signatures.some(signature => {
      const config = this.parameterType(signature, position);
      return config !== undefined && this.declaredProperty(config, bodyKey) !== undefined;
    });
    return declared ? VERIFIED : failed('key_missing');
  }

  /**
   * The callee's signatures a request claim can be read through. The callee
   * is `receiver[member]`, or the receiver itself when `member` is null.
   *
   * `config`: the first parameter declares `url_key` accepting string and
   * `method_key` accepting string or an HTTP method literal. `path_options`:
   * the first parameter accepts string and the second declares `method_key`
   * accepting the same. With no keys given (the `request_body` selection) only
   * the parameter shape is required.
   */
  private selectRequest(
    receiver: ts.Type,
    member: string | null,
    args: 'config' | 'path_options',
    keys: { urlKey?: string; methodKey?: string }
  ): { signatures: readonly ts.Signature[] } | { failure: Outcome } {
    const callable =
      member === null ? this.callSignatures(receiver) : this.callableProperty(receiver, member);
    if ('reason' in callable) return { failure: failed(callable.reason) };
    const checkKeys = keys.methodKey !== undefined;

    const selected: ts.Signature[] = [];
    let best: RankedFailure | undefined;
    for (const signature of callable.value) {
      const first = this.parameterType(signature, 0);
      let config: ts.Type | undefined;
      if (args === 'config') {
        config = first;
      } else {
        const second = this.parameterType(signature, 1);
        config = first !== undefined && this.acceptsString(first) ? second : undefined;
      }
      if (!config) {
        best = furthest(best, 1, failed('param_missing'));
        continue;
      }
      if (checkKeys) {
        const url =
          args === 'config'
            ? keys.urlKey === undefined
              ? undefined
              : this.declaredProperty(config, keys.urlKey)
            : null;
        const method = this.declaredProperty(config, keys.methodKey!);
        if (url === undefined || !method) {
          best = furthest(best, 2, failed('key_missing'));
          continue;
        }
        const urlAccepts = url === null || this.acceptsString(this.checker.getTypeOfSymbol(url));
        if (!urlAccepts || !this.acceptsMethod(this.checker.getTypeOfSymbol(method))) {
          best = furthest(best, 3, failed('key_not_string'));
          continue;
        }
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
   * A declared property `name` of `type` whose type has a call signature: the
   * checker finds it on the apparent type, with null and undefined removed.
   * An index signature does not count.
   */
  private callableProperty(type: ts.Type, name: string): Read<readonly ts.Signature[]> {
    const property = this.declaredProperty(type, name);
    if (!property) return { reason: 'member_missing' };
    return this.callSignatures(this.checker.getTypeOfSymbol(property));
  }

  private callSignatures(type: ts.Type): Read<readonly ts.Signature[]> {
    const signatures = this.checker.getNonNullableType(type).getCallSignatures();
    return signatures.length > 0 ? { value: signatures } : { reason: 'member_not_callable' };
  }

  private declaredProperty(type: ts.Type, name: string): ts.Symbol | undefined {
    return this.checker.getPropertyOfType(this.apparent(type), name);
  }

  private declaredProperties(type: ts.Type): ts.Symbol[] {
    return this.checker.getPropertiesOfType(this.apparent(type));
  }

  private apparent(type: ts.Type): ts.Type {
    return this.checker.getApparentType(this.checker.getNonNullableType(type));
  }

  /** `string` is assignable to the type (null and undefined never matter). */
  private acceptsString(type: ts.Type): boolean {
    return this.checker.isTypeAssignableTo(this.checker.getStringType(), type);
  }

  /** Accepts string, or names an HTTP method as a literal in either case. */
  private acceptsMethod(type: ts.Type): boolean {
    if (this.acceptsString(type)) return true;
    return constituents(type).some(
      part => part.isStringLiteral() && HTTP_METHODS.has(part.value.toUpperCase())
    );
  }

  /** Every part besides null and undefined is an object type. */
  private isObjectType(type: ts.Type): boolean {
    const parts = constituents(type).filter(part => !isNullish(part));
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
   * The type of the signature's parameter at `index`, reading through a rest
   * parameter; `undefined` when the signature has no parameter there.
   */
  private parameterType(signature: ts.Signature, index: number): ts.Type | undefined {
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
    return rest;
  }
}

/** `any` or `unknown`: a type that says nothing. */
function isOpenTop(type: ts.Type): boolean {
  return (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown)) !== 0;
}

/** A type parameter, `any` or `unknown`, once null and undefined are removed. */
function isOpen(type: ts.Type): boolean {
  const parts = constituents(type).filter(part => !isNullish(part));
  return (
    parts.length > 0 &&
    parts.every(part => (part.flags & (ts.TypeFlags.TypeParameter | ts.TypeFlags.Any | ts.TypeFlags.Unknown)) !== 0)
  );
}

function isNullish(type: ts.Type): boolean {
  return (type.flags & (ts.TypeFlags.Null | ts.TypeFlags.Undefined | ts.TypeFlags.Void)) !== 0;
}

function constituents(type: ts.Type): readonly ts.Type[] {
  return type.isUnion() ? type.types : [type];
}
