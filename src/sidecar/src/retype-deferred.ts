/**
 * Whether the retype's "the call's type argument does not reach its result"
 * can be answered before the rewrite for a call whose result stays deferred
 * (carrick#3165, tier 2 of carrick#3158), for every stated type that has no
 * top-level `undefined`.
 *
 * The shape is a generated client's method call: the first type argument is
 * the response map, a later one is the enclosing function's own free type
 * parameter, so the result is `Awaited` of a conditional that stays deferred.
 * The reach test reads the result, its alias argument and the members of its
 * apparent type. Each member is a union of the awaited arms' member types, and
 * holds an `undefined` the stated type never put there, so it is identical to
 * the stated type only when the stated type has a top-level `undefined`. The
 * scanner applies the answer only when the producer's text has none
 * (`states_no_top_level_undefined`).
 *
 * Reads only the consumer's code. Every rule fails closed: a shape this file
 * does not understand answers `false` and the call goes to the full retype,
 * which is unchanged.
 *
 *  T0  the program is strict (`strictNullChecks`): without it `X | undefined`
 *      is `X` and the witness below is empty.
 *  T1  one signature (tier 1's C0) and a written type argument.
 *  T2  the first type parameter is `D` (the stated type). A later one is
 *      `free` (a bare type parameter written at the call) or `closed` (a
 *      written argument, or a default, naming no type parameter).
 *  T3  walk the return type. `D` never sits at a structural position, only
 *      inside the type of a member, which is recorded.
 *  T4  the result is the default library's `Awaited` of a conditional.
 *  T5  every property the test reads has an arm whose declaration names no
 *      `D` and whose type holds `undefined`.
 */

import { ts } from 'ts-morph';
import type { CallExpression } from 'ts-morph';
import {
  anyChild,
  holdsInfer,
  isClosedType,
  isTypeParameter,
  referencedSymbol,
  singleSignatureDeclaration,
} from './retype-selector.js';

type Role = 'D' | 'closed' | 'free';

interface Env {
  roles: Map<ts.Symbol, Role>;
  /** The written type of a `closed` parameter that names no type parameter. */
  values: Map<ts.Symbol, ts.TypeNode>;
}

/** A shape the rule does not read: the call is left to the full retype. */
class Refuse extends Error {}

const MAX_DEPTH = 64;

export function deferredResultNeedsUndefined(call: CallExpression): boolean {
  try {
    return decide(call);
  } catch {
    return false;
  }
}

function decide(call: CallExpression): boolean {
  const project = call.getProject();
  const program = project.getProgram().compilerObject;
  const checker = project.getTypeChecker().compilerObject;

  // T0.
  const options = program.getCompilerOptions();
  if ((options.strictNullChecks ?? options.strict ?? false) !== true) return false;

  // T1.
  const typeArgs = call.getTypeArguments();
  if (typeArgs.length === 0) return false;
  const declaration = singleSignatureDeclaration(call);
  const parameters = declaration?.typeParameters;
  if (!declaration || !parameters || typeArgs.length > parameters.length) return false;

  // T2.
  const env: Env = { roles: new Map(), values: new Map() };
  for (let i = 0; i < parameters.length; i++) {
    const symbol = checker.getSymbolAtLocation(parameters[i].name);
    if (!symbol) return false;
    if (i === 0) {
      env.roles.set(symbol, 'D');
      continue;
    }
    const written = i < typeArgs.length ? typeArgs[i].compilerNode : undefined;
    const source = written ?? parameters[i].default;
    if (!source) return false;
    if (written && isBareTypeParameter(written, checker)) {
      env.roles.set(symbol, 'free');
    } else if (isClosedType(source, checker)) {
      env.roles.set(symbol, 'closed');
      env.values.set(symbol, source);
    } else {
      return false;
    }
  }

  // T3.
  const recorded = new Map<ts.Node, boolean>();
  new Walk(checker, program, recorded).walk(declaration.type, env, 0);

  // T4.
  const callType = call.getType().compilerType;
  const result = checker.getAwaitedType(callType) ?? callType;
  if (!(result.flags & ts.TypeFlags.Conditional)) return false;
  const alias = result.aliasSymbol;
  if (
    alias?.escapedName !== 'Awaited' ||
    !alias.declarations?.length ||
    !alias.declarations.every((d) => program.isSourceFileDefaultLibrary(d.getSourceFile()))
  ) {
    return false;
  }
  if (
    result.aliasTypeArguments?.length !== 1 ||
    !(result.aliasTypeArguments[0].flags & ts.TypeFlags.Conditional)
  ) {
    return false;
  }

  // T5.
  const apparent = checker.getApparentType(result);
  const arms = apparent.isUnion() ? apparent.types : [apparent];
  for (const property of checker.getPropertiesOfType(result)) {
    const witnessed = arms.some((arm) => {
      const member = checker.getPropertyOfType(arm, property.getName());
      if (!member || member.declarations?.length !== 1) return false;
      if (recorded.get(member.declarations[0]) !== false) return false;
      const type = checker.getTypeOfSymbol(member);
      return (type.isUnion() ? type.types : [type]).some((t) => (t.flags & ts.TypeFlags.Undefined) !== 0);
    });
    if (!witnessed) return false;
  }
  return true;
}

/** A bare reference (no type arguments) to a type parameter. */
function isBareTypeParameter(node: ts.TypeNode, checker: ts.TypeChecker): boolean {
  return ts.isTypeReferenceNode(node) && !node.typeArguments?.length && isTypeParameter(node, checker);
}

class Walk {
  constructor(
    private readonly checker: ts.TypeChecker,
    private readonly program: ts.Program,
    private readonly recorded: Map<ts.Node, boolean>
  ) {}

  /** Whether any type reference in `node` is a parameter whose role is `D`. */
  private namesD(node: ts.Node, env: Env): boolean {
    if (ts.isTypeReferenceNode(node)) {
      const symbol = referencedSymbol(node, this.checker);
      if (symbol && env.roles.get(symbol) === 'D') return true;
    }
    return anyChild(node, (child) => this.namesD(child, env));
  }

  /** The symbol of a bare reference to a parameter `env` tracks. */
  private bareTracked(node: ts.Node, env: Env): ts.Symbol | undefined {
    if (ts.isParenthesizedTypeNode(node)) return this.bareTracked(node.type, env);
    if (!ts.isTypeReferenceNode(node) || node.typeArguments?.length) return undefined;
    const symbol = referencedSymbol(node, this.checker);
    return symbol && env.roles.has(symbol) ? symbol : undefined;
  }

  walk(node: ts.TypeNode, env: Env, depth: number): void {
    if (depth > MAX_DEPTH) throw new Refuse('too deep');
    if (ts.isParenthesizedTypeNode(node)) return this.walk(node.type, env, depth + 1);
    if (ts.isUnionTypeNode(node)) {
      for (const member of node.types) this.walk(member, env, depth + 1);
      return;
    }
    if (ts.isIntersectionTypeNode(node)) return this.intersection(node, env, depth);
    if (ts.isTypeLiteralNode(node)) return this.literal(node, env);
    if (ts.isConditionalTypeNode(node)) return this.conditional(node, env, depth);
    if (ts.isTypeReferenceNode(node)) return this.reference(node, env, depth);
    if (node.kind === ts.SyntaxKind.UndefinedKeyword) return;
    if (ts.isLiteralTypeNode(node) && node.literal.kind === ts.SyntaxKind.NullKeyword) return;
    throw new Refuse(`node kind ${ts.SyntaxKind[node.kind]} at a structural position`);
  }

  /** Every member is a property signature; each is recorded, not walked. */
  private literal(node: ts.TypeLiteralNode, env: Env): void {
    for (const member of node.members) {
      if (!ts.isPropertySignature(member)) throw new Refuse('a member that is not a property');
      const name = propertyName(member);
      if (name === undefined || name === 'then') throw new Refuse('a member the walk cannot read');
      this.recorded.set(
        member,
        (this.recorded.get(member) ?? false) || (!!member.type && this.namesD(member.type, env))
      );
    }
  }

  private intersection(node: ts.IntersectionTypeNode, env: Env, depth: number): void {
    const seen = new Set<string>();
    for (const operand of node.types) {
      for (const name of this.operandNames(operand, 0)) {
        if (seen.has(name)) throw new Refuse(`'${name}' is declared by two members`);
        seen.add(name);
      }
      this.walk(operand, env, depth + 1);
    }
  }

  /**
   * The property names an intersection member declares, reached through
   * parentheses, alias references and unions to type literals only.
   */
  private operandNames(node: ts.TypeNode, depth: number): Set<string> {
    if (depth > MAX_DEPTH) throw new Refuse('too deep');
    if (ts.isParenthesizedTypeNode(node)) return this.operandNames(node.type, depth + 1);
    if (ts.isUnionTypeNode(node)) {
      const names = new Set<string>();
      for (const member of node.types) {
        for (const name of this.operandNames(member, depth + 1)) names.add(name);
      }
      return names;
    }
    if (ts.isTypeLiteralNode(node)) {
      const names = new Set<string>();
      for (const member of node.members) {
        const name = ts.isPropertySignature(member) ? propertyName(member) : undefined;
        if (name === undefined) throw new Refuse('a member the walk cannot read');
        names.add(name);
      }
      return names;
    }
    if (ts.isTypeReferenceNode(node)) {
      const declaration = this.aliasDeclaration(referencedSymbol(node, this.checker));
      if (declaration) return this.operandNames(declaration.type, depth + 1);
    }
    throw new Refuse('an intersection member that is not a type literal');
  }

  private conditional(node: ts.ConditionalTypeNode, env: Env, depth: number): void {
    if (this.namesD(node.checkType, env) || this.namesD(node.extendsType, env)) {
      throw new Refuse('a selector names the stated type');
    }
    if (holdsInfer(node.extendsType)) throw new Refuse('infer');
    // A closed selector on a parameter whose value is a known concrete type
    // takes one branch, the same for any stated type.
    const selector = this.bareTracked(node.checkType, env);
    const value = selector ? env.values.get(selector) : undefined;
    if (selector && value && env.roles.get(selector) === 'closed' && isClosedType(node.extendsType, this.checker)) {
      const checked = this.checker.getTypeFromTypeNode(value);
      const against = this.checker.getTypeFromTypeNode(node.extendsType);
      if (
        !checked.isUnion() &&
        !(checked.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.Never))
      ) {
        const taken = this.checker.isTypeAssignableTo(checked, against) ? node.trueType : node.falseType;
        return this.walk(taken, env, depth + 1);
      }
    }
    this.walk(node.trueType, env, depth + 1);
    this.walk(node.falseType, env, depth + 1);
  }

  private reference(node: ts.TypeReferenceNode, env: Env, depth: number): void {
    const symbol = referencedSymbol(node, this.checker);
    if (!symbol) throw new Refuse('an unresolved reference');
    if (symbol.flags & ts.SymbolFlags.TypeParameter) throw new Refuse('a type parameter at a structural position');

    const declaration = this.aliasDeclaration(symbol);
    if (declaration) return this.alias(node, declaration, env, depth);

    const isLibraryPromise =
      symbol.escapedName === 'Promise' &&
      !!symbol.declarations?.length &&
      symbol.declarations.every((d) => this.program.isSourceFileDefaultLibrary(d.getSourceFile()));
    if (isLibraryPromise && node.typeArguments?.length === 1) {
      return this.walk(node.typeArguments[0], env, depth + 1);
    }
    throw new Refuse(`a reference to '${symbol.escapedName}' at a structural position`);
  }

  /** The one declaration of a type alias this walk can read. */
  private aliasDeclaration(symbol: ts.Symbol | undefined): ts.TypeAliasDeclaration | undefined {
    if (!symbol || !(symbol.flags & ts.SymbolFlags.TypeAlias)) return undefined;
    const declaration = symbol.declarations?.[0];
    if (symbol.declarations?.length !== 1 || !declaration || !ts.isTypeAliasDeclaration(declaration)) {
      return undefined;
    }
    return declaration;
  }

  /** Map the alias's parameters from this reference's arguments, then walk its body. */
  private alias(node: ts.TypeReferenceNode, declaration: ts.TypeAliasDeclaration, env: Env, depth: number): void {
    const parameters = declaration.typeParameters ?? [];
    const args = node.typeArguments ?? [];
    if (args.length > parameters.length) throw new Refuse('too many alias arguments');
    const inner: Env = { roles: new Map(), values: new Map() };
    for (let i = 0; i < parameters.length; i++) {
      const parameter = this.checker.getSymbolAtLocation(parameters[i].name);
      if (!parameter) throw new Refuse('an unreadable alias parameter');
      const written = i < args.length ? args[i] : undefined;
      const source = written ?? parameters[i].default;
      if (!source) throw new Refuse('a missing alias argument');
      // A written argument is read where it was written; a default, against
      // the alias's own earlier parameters.
      const scope = written ? env : inner;
      const tracked = this.bareTracked(source, scope);
      if (tracked) {
        inner.roles.set(parameter, scope.roles.get(tracked)!);
        const value = scope.values.get(tracked);
        if (value) inner.values.set(parameter, value);
      } else if (this.namesD(source, scope)) {
        throw new Refuse('an alias argument built from the stated type');
      } else {
        inner.roles.set(parameter, 'closed');
        if (isClosedType(source, this.checker)) inner.values.set(parameter, source);
      }
    }
    this.walk(declaration.type, inner, depth + 1);
  }
}

function propertyName(member: ts.PropertySignature): string | undefined {
  const name = member.name;
  if (ts.isIdentifier(name) || ts.isStringLiteral(name)) return name.text;
  return undefined;
}
