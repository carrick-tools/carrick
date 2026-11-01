/**
 * Whether the type argument a retype fills can only SELECT (carrick#3158): in
 * the call's result it appears as a conditional's check or extends type, or
 * not at all. Such a call's result does not depend on the type stated, as
 * long as the stated type is not a union, so the retype's "the call's type
 * argument does not reach its result" can be answered before the rewrite for
 * any stated type that is a new object type.
 *
 * Reads only the consumer's code. Every rule fails closed: a shape this
 * file does not understand answers `false` and the call goes to the full
 * retype, which is unchanged.
 *
 *  C0  one signature, with type parameters and a written return type.
 *  C1  every type parameter after the first is closed: a written argument
 *      (explicit branch) or a default naming no type parameter. When the
 *      stated type breaks the first parameter's constraint, TypeScript fills
 *      unwritten arguments with their raw defaults, so a default that names
 *      a type parameter would change the result.
 *  C3  the call sits in no generic declaration: a producer type naming a
 *      type parameter in scope would defer the conditional.
 *  C2  the first parameter appears in the return type only in the check or
 *      extends type of a conditional that resolves (extends type without
 *      `infer`, only tracked type parameters named in either type).
 */

import { Node, ts } from 'ts-morph';
import type { CallExpression } from 'ts-morph';

type Role = 'D' | 'closed';
type Roles = Map<ts.Symbol, Role>;

const MAX_DEPTH = 64;

export function typeArgumentOnlySelects(call: CallExpression): boolean {
  // A producer type may name a type parameter in scope at the call. The
  // conditional then defers and the stated type reaches the result, so the
  // full retype must judge it.
  if (call.getAncestors().some((a) => Node.isTypeParametered(a) && a.getTypeParameters().length > 0)) {
    return false;
  }
  const typeArgs = call.getTypeArguments();
  const checker = call.getProject().getTypeChecker().compilerObject;
  const declaration = checker.getResolvedSignature(call.compilerNode)?.getDeclaration() as
    | ts.SignatureDeclaration
    | undefined;
  const parameters = declaration?.typeParameters;
  if (!declaration || !parameters || parameters.length === 0 || !declaration.type) return false;

  // C0: one signature on a callee that is not a union.
  const callee = call.getExpression().getType();
  if (callee.isUnion() || callee.getCallSignatures().length !== 1) return false;

  const explicit = typeArgs.length > 0;
  if (typeArgs.length > parameters.length) return false;

  // C1.
  const roles: Roles = new Map();
  for (let i = 0; i < parameters.length; i++) {
    const symbol = checker.getSymbolAtLocation(parameters[i].name);
    if (!symbol) return false;
    if (i === 0) {
      roles.set(symbol, 'D');
      continue;
    }
    const written = explicit && i < typeArgs.length ? typeArgs[i].compilerNode : undefined;
    const source = written ?? parameters[i].default;
    if (!source || !isClosedType(source, checker)) return false;
    roles.set(symbol, 'closed');
  }

  // C2.
  return selectsOnly(declaration.type, roles, checker, new Set(), 0);
}

/** Whether `test` holds for any direct child (`forEachChild` stops only on a truthy return). */
function anyChild(node: ts.Node, test: (child: ts.Node) => boolean): boolean {
  return ts.forEachChild(node, (child) => (test(child) ? true : undefined)) ?? false;
}

/** Names no type parameter, no `this` and no `typeof`. */
function isClosedType(node: ts.Node, checker: ts.TypeChecker): boolean {
  if (ts.isTypeQueryNode(node) || ts.isThisTypeNode(node)) return false;
  if (ts.isTypeReferenceNode(node) && isTypeParameter(node, checker)) return false;
  return !anyChild(node, (child) => !isClosedType(child, checker));
}

function referencedSymbol(
  node: ts.TypeReferenceNode,
  checker: ts.TypeChecker
): ts.Symbol | undefined {
  const name = ts.isQualifiedName(node.typeName) ? node.typeName.right : node.typeName;
  const symbol = checker.getSymbolAtLocation(name);
  if (symbol && symbol.flags & ts.SymbolFlags.Alias) {
    try {
      return checker.getAliasedSymbol(symbol);
    } catch {
      return symbol;
    }
  }
  return symbol;
}

function isTypeParameter(node: ts.TypeReferenceNode, checker: ts.TypeChecker): boolean {
  const symbol = referencedSymbol(node, checker);
  return !!symbol && (symbol.flags & ts.SymbolFlags.TypeParameter) !== 0;
}

/** Whether `node` names a type parameter whose role is `D`. */
function namesD(node: ts.Node, roles: Roles, checker: ts.TypeChecker): boolean {
  if (ts.isTypeReferenceNode(node)) {
    const symbol = referencedSymbol(node, checker);
    if (symbol && roles.get(symbol) === 'D') return true;
  }
  return anyChild(node, (child) => namesD(child, roles, checker));
}

/** A conditional's check or extends type: tracked parameters only. */
function namesOnlyTracked(node: ts.Node, roles: Roles, checker: ts.TypeChecker): boolean {
  if (ts.isThisTypeNode(node) || ts.isTypeQueryNode(node)) return false;
  if (ts.isTypeReferenceNode(node)) {
    const symbol = referencedSymbol(node, checker);
    if (symbol && symbol.flags & ts.SymbolFlags.TypeParameter && !roles.has(symbol)) return false;
  }
  return !anyChild(node, (child) => !namesOnlyTracked(child, roles, checker));
}

function holdsInfer(node: ts.Node): boolean {
  if (ts.isInferTypeNode(node)) return true;
  return anyChild(node, holdsInfer);
}

/** C2: `D` reaches nothing in `node` except as a resolving conditional's selector. */
function selectsOnly(
  node: ts.Node,
  roles: Roles,
  checker: ts.TypeChecker,
  visiting: Set<ts.Symbol>,
  depth: number
): boolean {
  if (depth > MAX_DEPTH) return false;
  if (ts.isTypeQueryNode(node)) return false;

  if (ts.isConditionalTypeNode(node) && namesD(node, roles, checker)) {
    if (holdsInfer(node.extendsType)) return false;
    if (
      !namesOnlyTracked(node.checkType, roles, checker) ||
      !namesOnlyTracked(node.extendsType, roles, checker)
    ) {
      return false;
    }
    return (
      selectsOnly(node.trueType, roles, checker, visiting, depth + 1) &&
      selectsOnly(node.falseType, roles, checker, visiting, depth + 1)
    );
  }

  if (ts.isTypeReferenceNode(node)) {
    const symbol = referencedSymbol(node, checker);
    if (!symbol) return !namesD(node, roles, checker);
    if (symbol.flags & ts.SymbolFlags.TypeParameter) return roles.get(symbol) !== 'D';
    if (symbol.flags & ts.SymbolFlags.TypeAlias) {
      if (!namesD(node, roles, checker)) return true;
      return aliasSelectsOnly(node, symbol, roles, checker, visiting, depth);
    }
    // An interface, a class or an enum: its arguments are read like any node.
  }

  return !anyChild(node, (child) => !selectsOnly(child, roles, checker, visiting, depth + 1));
}

/** A reference to a type alias whose arguments name `D`: walk the alias. */
function aliasSelectsOnly(
  node: ts.TypeReferenceNode,
  symbol: ts.Symbol,
  roles: Roles,
  checker: ts.TypeChecker,
  visiting: Set<ts.Symbol>,
  depth: number
): boolean {
  // A JSDoc typedef, or an alias this cannot read, has `D` among its
  // arguments: not decided.
  const declaration = symbol.declarations?.find(ts.isTypeAliasDeclaration);
  if (!declaration || symbol.declarations?.length !== 1) return false;
  if (visiting.has(symbol)) return false;

  const parameters = declaration.typeParameters ?? [];
  const args = node.typeArguments ?? [];
  if (args.length > parameters.length) return false;
  const inner: Roles = new Map();
  for (let i = 0; i < parameters.length; i++) {
    const parameter = checker.getSymbolAtLocation(parameters[i].name);
    if (!parameter) return false;
    const written = i < args.length ? args[i] : undefined;
    const source = written ?? parameters[i].default;
    if (!source) return false;
    // The scope the argument is read in: the caller's, or the alias's own for
    // a default.
    const scope = written ? roles : inner;
    if (!namesD(source, scope, checker)) {
      inner.set(parameter, 'closed');
    } else if (bareD(source, scope, checker)) {
      inner.set(parameter, 'D');
    } else {
      return false;
    }
  }
  visiting.add(symbol);
  const ok = selectsOnly(declaration.type, inner, checker, visiting, depth + 1);
  visiting.delete(symbol);
  return ok;
}

/** `D` itself, not a type built from it. */
function bareD(node: ts.Node, roles: Roles, checker: ts.TypeChecker): boolean {
  if (ts.isParenthesizedTypeNode(node)) return bareD(node.type, roles, checker);
  if (!ts.isTypeReferenceNode(node) || node.typeArguments?.length) return false;
  const symbol = referencedSymbol(node, checker);
  return !!symbol && roles.get(symbol) === 'D';
}
