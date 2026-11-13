/**
 * Type parameters a read type still holds open, and the type a call
 * instantiates (carrick#3897, carrick#4128). A type read inside a generic
 * function names that function's type parameters; printed into a contract
 * they name nothing. Its callers say what they are.
 */

import { ts } from 'ts-morph';

/** How deep {@link enclosingTypeParameterIn} reads into a type. */
const TYPE_PARAMETER_DEPTH = 4;

/**
 * The name of a type parameter in `type` that a declaration enclosing `at`
 * declares (a function, a method, a class), or `undefined`. Reads union and
 * intersection members, type arguments, indexed accesses, conditional
 * branches and the properties of an anonymous object type, to a fixed depth.
 * A polymorphic `this` is not one: a call can hold it without passing a
 * response through.
 */
export function enclosingTypeParameterIn(
  checker: ts.TypeChecker,
  type: ts.Type,
  at: ts.Node
): string | undefined {
  const seen = new Set<ts.Type>();
  const encloses = (declaring: ts.Node): boolean => {
    for (let node: ts.Node | undefined = at.parent; node; node = node.parent) {
      if (node === declaring) return true;
    }
    return false;
  };
  const visit = (current: ts.Type, depth: number): string | undefined => {
    if (depth > TYPE_PARAMETER_DEPTH || seen.has(current)) return undefined;
    seen.add(current);
    if (current.flags & ts.TypeFlags.TypeParameter) {
      if ((current as ts.TypeParameter & { isThisType?: boolean }).isThisType) return undefined;
      const declared = current.symbol?.declarations?.some(
        (decl) => ts.isTypeParameterDeclaration(decl) && encloses(decl.parent)
      );
      if (declared) return current.symbol.getName();
    }
    const parts: ts.Type[] = [...(current.aliasTypeArguments ?? [])];
    if (current.isUnionOrIntersection()) parts.push(...current.types);
    if (current.flags & ts.TypeFlags.IndexedAccess) {
      const access = current as ts.IndexedAccessType;
      parts.push(access.objectType, access.indexType);
    }
    if (current.flags & ts.TypeFlags.Conditional) {
      const conditional = current as ts.ConditionalType;
      parts.push(conditional.checkType, conditional.extendsType);
    }
    if (current.flags & ts.TypeFlags.Object) {
      const flags = (current as ts.ObjectType).objectFlags;
      if (flags & ts.ObjectFlags.Reference) {
        parts.push(...checker.getTypeArguments(current as ts.TypeReference));
      }
      if (flags & ts.ObjectFlags.Anonymous) {
        for (const property of current.getProperties()) {
          parts.push(checker.getTypeOfSymbolAtLocation(property, at));
        }
      }
    }
    for (const part of parts) {
      const found = visit(part, depth + 1);
      if (found !== undefined) return found;
    }
    return undefined;
  };
  return visit(type, 0);
}

/** `node` sits inside `declaration`. */
function sitsIn(node: ts.Node, declaration: ts.Node): boolean {
  for (let at: ts.Node | undefined = node.parent; at; at = at.parent) {
    if (at === declaration) return true;
  }
  return false;
}

/** The checker members the instantiation below reads, which its typings leave out. */
interface InstantiatingChecker {
  createSymbol(flags: ts.SymbolFlags, name: string, checkFlags?: number): ts.Symbol;
}
type TransientLinks = { links: { type?: ts.Type; target?: ts.Symbol; mapper?: unknown } };
/** The checker's `CheckFlags.Instantiated`: a symbol whose type is its target's, mapped. */
const INSTANTIATED = 1;

/**
 * How `call` reads the type `type` takes at `value` (carrick#4128):
 *
 *  - `as_read`: `type` holds no type parameter of a declaration around
 *    `value`, so there is nothing to instantiate;
 *  - `instantiated`: a symbol whose type is `type` with the type arguments
 *    of `call`'s resolved signature put in, where `value` sits in the
 *    function `call` resolves to and the result holds no open parameter of
 *    either side;
 *  - `open`: the name of a type parameter left open, where `call` does not
 *    reach the function `value` is in, or binds the parameter only to one
 *    of its own caller's (`<U>(u: U) => place(u)`).
 *
 * The symbol is handed back rather than its type so that a reader holding
 * its own wrapper over the checker reads it there.
 */
export function instantiatedAtCall(
  checker: ts.TypeChecker,
  type: ts.Type,
  value: ts.Node,
  call: ts.CallLikeExpression
):
  | { kind: 'as_read' }
  | { kind: 'instantiated'; symbol: ts.Symbol }
  | { kind: 'open'; name: string } {
  const open = enclosingTypeParameterIn(checker, type, value);
  if (open === undefined) return { kind: 'as_read' };
  const signature = checker.getResolvedSignature(call);
  const mapper = (signature as (ts.Signature & { mapper?: unknown }) | undefined)?.mapper;
  const declaration = signature?.getDeclaration() as ts.Node | undefined;
  if (mapper === undefined || !declaration || !sitsIn(value, declaration)) {
    return { kind: 'open', name: open };
  }
  let symbol: ts.Symbol;
  let instantiated: ts.Type;
  try {
    // The checker's public surface has no `instantiateType`: a transient
    // symbol marked instantiated, whose target holds `type` and whose mapper
    // is the call's, is read through `getTypeOfSymbol`, which instantiates it.
    const internal = checker as unknown as InstantiatingChecker;
    const carrier = internal.createSymbol(ts.SymbolFlags.Property, '__read');
    (carrier as unknown as TransientLinks).links.type = type;
    symbol = internal.createSymbol(ts.SymbolFlags.Property, '__instantiated', INSTANTIATED);
    const links = (symbol as unknown as TransientLinks).links;
    links.target = carrier;
    links.mapper = mapper;
    instantiated = checker.getTypeOfSymbol(symbol);
  } catch {
    return { kind: 'open', name: open };
  }
  const still =
    enclosingTypeParameterIn(checker, instantiated, value) ??
    enclosingTypeParameterIn(checker, instantiated, call);
  return still === undefined ? { kind: 'instantiated', symbol } : { kind: 'open', name: still };
}
