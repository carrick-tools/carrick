/**
 * Which parts of a call's return could hold the response body the call
 * resolves to (carrick#2817). The compiler decides one case, that the
 * caller supplies the whole return as a type argument (`whole_decided`); for
 * every other return it only lists the candidates: the members that carry a
 * type parameter of the signature, and the parse methods. Choosing among them
 * is the store picker's job, and `holdsResult` judges the choice.
 *
 * Rule R, per signature `S` with type parameters `P`:
 * 1. `A` is the awaited return of `S`. While `A` is a type parameter of `S`
 *    whose default is not `unknown` or `any`, take the default (awaited), at
 *    most four hops: a default of `unknown`/`any` is the caller-supplies-it
 *    case.
 * 2. `whole_decided`: `A` is a type parameter of `S` with no constraint.
 * 3. `members`: across the branches of `A` (a conditional's true and false
 *    types, a union's constituents without `undefined`/`null`/`void`; depth at
 *    most four), every property with no call signature whose declared type
 *    mentions a type parameter in `P`.
 * 4. `parse`: properties of `S`'s raw return or of a branch of `A` that can
 *    be called with no required argument and whose awaited return is a bare,
 *    unconstrained type parameter of that method's own signature.
 * A signature that returns nothing (void, undefined, never) has no candidates.
 */

import { ts } from 'ts-morph';
import type { ResultShape, SurfaceResultCandidates } from './types.js';

const MAX_DEPTH = 4;
const NOTHING = ts.TypeFlags.Void | ts.TypeFlags.Undefined | ts.TypeFlags.Never;
const NULLISH = ts.TypeFlags.Undefined | ts.TypeFlags.Null | ts.TypeFlags.Void;
const OPEN = ts.TypeFlags.Any | ts.TypeFlags.Unknown;

const isTypeParameter = (type: ts.Type): type is ts.TypeParameter => (type.flags & ts.TypeFlags.TypeParameter) !== 0;

const byCodeUnit = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0);

export function resultCandidatesOf(checker: ts.TypeChecker, signature: ts.Signature): SurfaceResultCandidates | undefined {
  return resultCandidatesOfReturn(checker, checker.getReturnTypeOfSignature(signature), signature.getTypeParameters() ?? []);
}

/**
 * Rule R over a return type read against `parameters`: the type parameters
 * that stand for the caller's value. A handler a route takes returns a type
 * the route's own parameters decide (carrick#3047), so those are the ones
 * its return is read against.
 */
export function resultCandidatesOfReturn(
  checker: ts.TypeChecker,
  returned: ts.Type,
  parameters: readonly ts.Type[]
): SurfaceResultCandidates | undefined {
  return new ResultReader(checker).candidates(returned, parameters);
}

/** Whether `shape` names one of `candidates`. `whole` is always one; a decided whole is the only one. */
export function holdsResult(candidates: SurfaceResultCandidates, shape: ResultShape): boolean {
  if ('whole' in shape) return true;
  if (candidates.whole_decided) return false;
  if ('member' in shape) return candidates.members.includes(shape.member[0]);
  return candidates.parse.includes(shape.parse);
}

/** A branch of a return, with the type parameters it counts as carrying a value of. */
interface Leaf {
  type: ts.Type;
  params: ReadonlySet<ts.Type>;
}

class ResultReader {
  constructor(private readonly checker: ts.TypeChecker) {}

  candidates(raw: ts.Type, parameters: readonly ts.Type[]): SurfaceResultCandidates | undefined {
    if (raw.flags & NOTHING) return undefined;
    const params = new Set<ts.Type>(parameters);

    let awaited = this.awaited(raw);
    for (let hop = 0; hop < MAX_DEPTH && isTypeParameter(awaited) && params.has(awaited); hop++) {
      const fallback = this.checker.getDefaultFromTypeParameter(awaited);
      if (fallback === undefined || fallback.flags & OPEN) break;
      awaited = this.awaited(fallback);
    }
    if (awaited.flags & NOTHING) return undefined;

    if (isTypeParameter(awaited) && params.has(awaited) && this.unconstrained(awaited)) {
      return { whole_decided: true, members: [], parse: [] };
    }

    const leaves: Leaf[] = [];
    this.leavesOf(awaited, params, 0, leaves);
    const members = new Set<string>();
    for (const leaf of leaves) {
      if (!(leaf.type.flags & (ts.TypeFlags.Object | ts.TypeFlags.Intersection))) continue;
      for (const property of this.checker.getPropertiesOfType(leaf.type)) {
        const type = this.checker.getTypeOfSymbol(property);
        if (this.checker.getNonNullableType(type).getCallSignatures().length > 0) continue;
        if (this.mentions(type, leaf.params, 0, new Set())) members.add(property.getName());
      }
    }

    const parse = new Set<string>();
    for (const holder of [raw, ...leaves.map(leaf => leaf.type)]) {
      if (!(holder.flags & (ts.TypeFlags.Object | ts.TypeFlags.Intersection))) continue;
      for (const property of this.checker.getPropertiesOfType(holder)) {
        const type = this.checker.getNonNullableType(this.checker.getTypeOfSymbol(property));
        if (type.getCallSignatures().some(method => this.parses(method))) parse.add(property.getName());
      }
    }

    return { whole_decided: false, members: [...members].sort(byCodeUnit), parse: [...parse].sort(byCodeUnit) };
  }

  /**
   * `type` with a promise unwrapped. The checker spells an open parameter's
   * awaited type `Awaited<T>`; that is read as `T`. A union and a conditional
   * are left to `leavesOf`, which awaits each arm.
   */
  private awaited(type: ts.Type, depth = 0): ts.Type {
    if (depth > MAX_DEPTH) return type;
    if (type.aliasSymbol?.getName() === 'Awaited' && type.aliasTypeArguments?.length === 1) {
      return this.awaited(type.aliasTypeArguments[0], depth + 1);
    }
    if (type.flags & (ts.TypeFlags.TypeParameter | ts.TypeFlags.Union | ts.TypeFlags.Conditional)) return type;
    const unwrapped = this.checker.getAwaitedType(type);
    return unwrapped === undefined || unwrapped === type ? type : this.awaited(unwrapped, depth + 1);
  }

  private unconstrained(parameter: ts.TypeParameter): boolean {
    const constraint = this.checker.getBaseConstraintOfType(parameter);
    return constraint === undefined || (constraint.flags & OPEN) !== 0;
  }

  /**
   * The branches of `type`: a union's parts without nullish ones, a
   * conditional's two arms, each awaited. `params` are the type parameters a
   * branch counts as carrying a value of: an arm of a generic alias is written
   * in the alias's own parameters, so the alias parameters applied to
   * something that mentions `params` join `params` for that arm.
   */
  private leavesOf(type: ts.Type, params: ReadonlySet<ts.Type>, depth: number, into: Leaf[]): void {
    if (depth > MAX_DEPTH) return;
    const current = this.awaited(type);
    if (current.flags & ts.TypeFlags.Union) {
      for (const part of (current as ts.UnionType).types) {
        if (!(part.flags & NULLISH)) this.leavesOf(part, params, depth + 1, into);
      }
      return;
    }
    if (current.flags & ts.TypeFlags.Conditional) {
      const node = (current as ts.ConditionalType).root.node;
      const armParams = this.aliasParams(current, params);
      for (const arm of [node.trueType, node.falseType]) {
        this.leavesOf(this.checker.getTypeFromTypeNode(arm), armParams, depth + 1, into);
      }
      return;
    }
    if (current.flags & NOTHING) return;
    into.push({ type: current, params });
  }

  /**
   * `params`, plus each type parameter of the alias `type` is an application
   * of whose argument mentions one of `params`. The arms of a conditional
   * written in an alias name those parameters, not the arguments.
   */
  private aliasParams(type: ts.Type, params: ReadonlySet<ts.Type>): ReadonlySet<ts.Type> {
    const declaration = type.aliasSymbol?.declarations?.find(ts.isTypeAliasDeclaration);
    const args = type.aliasTypeArguments;
    if (declaration === undefined || args === undefined) return params;
    const joined = new Set(params);
    (declaration.typeParameters ?? []).forEach((node, index) => {
      if (index < args.length && this.mentions(args[index], params, 0, new Set())) {
        joined.add(this.checker.getTypeAtLocation(node));
      }
    });
    return joined;
  }

  /** A parse method: no required argument, and it returns a bare unconstrained parameter of its own. */
  private parses(method: ts.Signature): boolean {
    const own = method.getTypeParameters() ?? [];
    if (own.length === 0) return false;
    const required = method.getParameters().some(parameter => {
      const declaration = parameter.valueDeclaration;
      if (declaration === undefined || !ts.isParameter(declaration)) return true;
      return declaration.questionToken === undefined && declaration.initializer === undefined && declaration.dotDotDotToken === undefined;
    });
    if (required) return false;
    const returned = this.awaited(this.checker.getReturnTypeOfSignature(method));
    return isTypeParameter(returned) && own.includes(returned) && this.unconstrained(returned);
  }

  /** Whether `type` names one of `params` anywhere a value of it could be read through. */
  private mentions(type: ts.Type, params: ReadonlySet<ts.Type>, depth: number, seen: Set<ts.Type>): boolean {
    if (params.has(type)) return true;
    if (depth > MAX_DEPTH || seen.has(type)) return false;
    seen.add(type);
    const next = (part: ts.Type): boolean => this.mentions(part, params, depth + 1, seen);
    if (type.aliasTypeArguments?.some(next)) return true;
    if (type.isUnionOrIntersection()) return type.types.some(next);
    if (type.flags & ts.TypeFlags.IndexedAccess) {
      const access = type as ts.IndexedAccessType;
      return next(access.objectType) || next(access.indexType);
    }
    if (type.flags & ts.TypeFlags.Index) return next((type as ts.IndexType).type);
    if (type.flags & ts.TypeFlags.Conditional) {
      const conditional = type as ts.ConditionalType;
      return next(conditional.checkType) || next(conditional.extendsType);
    }
    if (!(type.flags & ts.TypeFlags.Object)) return false;
    const object = type as ts.ObjectType;
    if (object.objectFlags & ts.ObjectFlags.Reference && this.checker.getTypeArguments(object as ts.TypeReference).some(next)) {
      return true;
    }
    if (this.checker.isArrayType(type) || this.checker.isTupleType(type)) return false;
    return this.checker.getPropertiesOfType(type).some(property => next(this.checker.getTypeOfSymbol(property)));
  }
}
