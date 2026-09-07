/**
 * Structural type expansion — renders a ts-morph `Type` as fully-inlined
 * structural text, with every named object/interface member expanded to its
 * member structure recursively.
 *
 * `Type.getText()` does NOT inline named members: the compiler prints a
 * referenced type by its symbol name when that symbol is in scope
 * (`total: Money`, not `total: { amountCents: number; currency: string }`).
 * That is fine inside a single project, but a cross-repo bundle carries only
 * the alias lines — no source declarations — so a named reference is a
 * dangling identifier that resolves to `any` downstream. Expanding the shape
 * structurally puts the real members in the bundle so the type checker can
 * compare them.
 *
 * Object/interface types are expanded to their members; primitives, literals,
 * library types (`Date`, `Promise`, tuples, …) and functions stay by name.
 * Bounded recursion + a per-branch cycle set guard against blow-ups; any type
 * that can't be safely expanded falls back to the non-expanded text rather
 * than throwing.
 *
 * Union and intersection members are printed in a canonical order that does not
 * depend on when the checker created each member type — see `orderMembers`
 * (carrick#735), which the depth backstop applies too (carrick#775).
 *
 * Shared by `definition-resolver.ts` (bundle alias resolution) and
 * `type-inferrer.ts` (consumer-side inference), so both paths emit the same
 * structural form rather than a dangling name.
 */

import { type Symbol, type Type, ts } from 'ts-morph';

/**
 * Bound on the structural-expansion recursion. Deep enough for every realistic
 * request/response shape; a backstop against pathological/recursive types the
 * cycle set somehow misses.
 */
export const MAX_EXPANSION_DEPTH = 12;

/**
 * Recursively render a `Type` as fully-inlined structural text.
 *
 * Named object/interface types are expanded to their member structure;
 * primitives, literals, library types (`Date`, `Promise`, tuples, …) and
 * functions stay by name. The `seen` set (object type ids on the current
 * branch) breaks reference cycles; `depth` is a hard backstop.
 */
export function expandTypeStructural(
  type: Type,
  seen: Set<number> = new Set(),
  depth = 0,
): string {
  if (depth > MAX_EXPANSION_DEPTH) return backstopText(type);

  // Primitives & literals: nothing to inline.
  if (
    type.isString() ||
    type.isNumber() ||
    type.isBoolean() ||
    type.isBooleanLiteral() ||
    type.isUndefined() ||
    type.isNull() ||
    type.isVoid() ||
    type.isAny() ||
    type.isUnknown() ||
    type.isNever() ||
    type.isStringLiteral() ||
    type.isNumberLiteral() ||
    type.isEnumLiteral()
  ) {
    return namedText(type);
  }

  // Unions / intersections: expand each member, in canonical order.
  if (type.isUnion()) {
    return canonicalMembers(type.getUnionTypes(), seen, depth).join(' | ');
  }
  if (type.isIntersection()) {
    return canonicalMembers(type.getIntersectionTypes(), seen, depth).join(
      ' & ',
    );
  }

  // Tuples are array-like but must keep their `[a, b]` shape, not be walked
  // as objects (which explodes into `Array.prototype`). Handle before arrays.
  if (isTuple(type)) {
    return namedText(type);
  }

  if (type.isArray()) {
    const element = type.getArrayElementType();
    if (!element) return namedText(type);
    const inner = expandTypeStructural(element, seen, depth + 1);
    // Parenthesise a union/intersection element so `(A | B)[]` doesn't misparse
    // as `A | B[]`. Decide from the TYPE, not the string: a single object
    // literal like `{ a: A | B }` is NOT a union and must not be parenthesised,
    // and a union led by an object literal (`{ a: string } | null`) MUST be.
    const needsParens = element.isUnion() || element.isIntersection();
    return needsParens ? `(${inner})[]` : `${inner}[]`;
  }

  // Library / built-in types (Date, Promise, RegExp, …): keep by name.
  if (isLibraryType(type)) {
    return namedText(type);
  }

  // Callable/constructable object types (functions): keep by name; their
  // structural form is the signature, which `getText` already renders.
  if (
    type.getCallSignatures().length > 0 ||
    type.getConstructSignatures().length > 0
  ) {
    return namedText(type);
  }

  if (type.isObject() || type.isInterface()) {
    const id = (type.compilerType as { id?: number }).id;
    if (id != null && seen.has(id)) return namedText(type);
    const nextSeen = id != null ? new Set(seen).add(id) : seen;

    const props = type.getProperties();
    if (props.length === 0) return namedText(type);

    const parts = props.map((prop) => expandProperty(prop, nextSeen, depth));
    return `{ ${parts.join('; ')}; }`;
  }

  return namedText(type);
}

/**
 * The print for a type the recursion bound stopped at (carrick#775).
 *
 * `namedText` is the compiler's own print, and for a union that print is in
 * type-id order — the creation-order artefact `canonicalMembers` exists to
 * remove. Stopping the recursion must not also stop the normalisation: a union
 * at depth 13 is as much a set as one at depth 2, and a diff reader comparing
 * two `expanded_definition` strings cannot tell which depth a member came from.
 *
 * So the members are rendered by NAME — no recursion, which is the whole point
 * of the bound — and put in the same canonical order as the expanded path.
 * Everything else falls through to the compiler's print unchanged.
 */
function backstopText(type: Type): string {
  if (type.isUnion()) {
    return orderMembers(type.getUnionTypes(), namedText).join(' | ');
  }
  if (type.isIntersection()) {
    return orderMembers(type.getIntersectionTypes(), namedText).join(' & ');
  }
  return namedText(type);
}

/**
 * Render every member of a union/intersection and put them in a canonical
 * order (carrick#735).
 *
 * The compiler stores a union's constituents sorted by type id, and ids are
 * handed out in the order the checker CREATES types. A literal type is created
 * the first time some declaration is checked, and it is then interned — so on
 * a tree where `"PENDING" | "TIMED_OUT"` and `"TIMED_OUT" | "PENDING"` are both
 * declared, whichever declaration is reached first decides the printed order of
 * BOTH. Two runs over an unchanged tree can therefore print one type two ways.
 *
 * The order carries no meaning: a union is a set, and the check phase compares
 * these strings by typechecking them, which is order-insensitive. So we impose
 * one:
 *
 *  - intrinsics (`string`, `null`, `undefined`, `number`, `true`/`false`, …)
 *    keep their compiler id order. Those ids are assigned when the checker is
 *    constructed, before a single source file is read, so their relative order
 *    is fixed for a given TypeScript version and cannot vary between runs.
 *    Keeping it means `string | null` still prints the way the compiler prints
 *    it, and only the unstable part of the order moves.
 *  - everything else (literals, objects, arrays, named types) sorts by its own
 *    rendered text, compared by UTF-16 code unit — a pure function of the
 *    member, with no dependence on when the checker happened to create it.
 *
 * Ties can only happen between two members that render identically, in which
 * case the joined output is the same whichever way round they go.
 */
function canonicalMembers(
  members: Type[],
  seen: Set<number>,
  depth: number,
): string[] {
  return orderMembers(members, (member) =>
    expandTypeStructural(member, seen, depth + 1),
  );
}

/**
 * The canonical order itself, over whatever text `render` gives each member.
 * Shared by the expanded path (`canonicalMembers`, which renders structurally)
 * and the depth backstop (`backstopText`, which renders by name), so one union
 * cannot be ordered two ways depending on how deep it sits.
 */
function orderMembers(
  members: Type[],
  render: (member: Type) => string,
): string[] {
  const rendered = members.map((member, index) => ({
    index,
    intrinsic: isIntrinsicType(member),
    id: (member.compilerType as { id?: number }).id ?? 0,
    text: render(member),
  }));
  rendered.sort((a, b) => {
    if (a.intrinsic !== b.intrinsic) return a.intrinsic ? -1 : 1;
    if (a.intrinsic && b.intrinsic) return a.id - b.id;
    if (a.text !== b.text) return a.text < b.text ? -1 : 1;
    return a.index - b.index;
  });
  return rendered.map((entry) => entry.text);
}

/**
 * True for the types the checker creates up front (`any`, `unknown`, `string`,
 * `number`, `bigint`, `boolean`/`true`/`false`, `symbol`, `void`, `undefined`,
 * `null`, `never`), whose ids — and therefore whose relative order inside a
 * union — are the same in every program. String/number/enum literal types are
 * NOT in this set: they are created on demand while checking, which is the
 * instability `canonicalMembers` normalises away.
 */
const INTRINSIC_TYPE_FLAGS =
  ts.TypeFlags.Any |
  ts.TypeFlags.Unknown |
  ts.TypeFlags.String |
  ts.TypeFlags.Number |
  ts.TypeFlags.BigInt |
  ts.TypeFlags.Boolean |
  ts.TypeFlags.BooleanLiteral |
  ts.TypeFlags.ESSymbol |
  ts.TypeFlags.Void |
  ts.TypeFlags.Undefined |
  ts.TypeFlags.Null |
  ts.TypeFlags.Never;

function isIntrinsicType(type: Type): boolean {
  return (type.getFlags() & INTRINSIC_TYPE_FLAGS) !== 0;
}

/** Render a single property as `name[?]: <expanded>`. */
function expandProperty(prop: Symbol, seen: Set<number>, depth: number): string {
  const optional = (prop.getFlags() & ts.SymbolFlags.Optional) !== 0;

  const propDecl = prop.getDeclarations()[0];
  // Render the key from the declaration's name node so quoted/computed keys
  // ('x-y', "x y", [Symbol.iterator]) survive as valid TS text rather than
  // being unquoted into invalid output; fall back to the bare symbol name.
  const name = renderPropertyName(prop, propDecl);
  let propType = propDecl
    ? prop.getTypeAtLocation(propDecl)
    : prop.getDeclaredType();

  // An optional property's type includes `undefined`; the structural label
  // drops it (`note?: string`, not `note?: string | undefined`).
  if (optional && propType.isUnion()) {
    const nonUndefined = propType
      .getUnionTypes()
      .filter((member) => !member.isUndefined());
    if (nonUndefined.length === 1) {
      propType = nonUndefined[0];
    } else if (nonUndefined.length > 1) {
      const inner = canonicalMembers(nonUndefined, seen, depth).join(' | ');
      return `${name}?: ${inner}`;
    }
  }

  const inner = expandTypeStructural(propType, seen, depth + 1);
  return `${name}${optional ? '?' : ''}: ${inner}`;
}

/**
 * The property key as valid TS text. Uses the declaration's name node so a
 * quoted (`'x-y'`) or computed (`[Symbol.iterator]`) key keeps its syntax;
 * `Symbol.getName()` would drop the quoting and emit invalid output. Falls
 * back to the bare symbol name when there's no usable name node.
 */
function renderPropertyName(prop: Symbol, decl: unknown): string {
  const node = decl as
    | { getNameNode?: () => { getText(): string } | undefined }
    | undefined;
  const text = node?.getNameNode?.()?.getText();
  return text && text.length > 0 ? text : prop.getName();
}

/** True for tuple types (`[a, b]`), which must not be walked as objects. */
function isTuple(type: Type): boolean {
  const compiler = type.compilerType as {
    objectFlags?: number;
    target?: { objectFlags?: number };
  };
  const target = compiler.target ?? compiler;
  return ((target.objectFlags ?? 0) & ts.ObjectFlags.Tuple) !== 0;
}

/**
 * True for types declared in `node_modules` or a TS `lib.*.d.ts` (Date,
 * Promise, RegExp, …). These stay by name rather than being inlined.
 */
function isLibraryType(type: Type): boolean {
  const symbol = type.getSymbol() ?? type.getAliasSymbol();
  if (!symbol) return false;
  const decls = symbol.getDeclarations();
  if (decls.length === 0) return false;
  return decls.some((decl) => {
    const sf = decl.getSourceFile();
    if (sf.isInNodeModules()) return true;
    return (
      sf.isDeclarationFile() &&
      // Normalize separators so a Windows `\\` path still matches lib.*.d.ts.
      /(^|\/)lib\.[^/]*\.d\.ts$/.test(sf.getFilePath().replace(/\\/g, '/'))
    );
  });
}

/**
 * Non-expanded text for a type. Passes `undefined` as the enclosing node so
 * the compiler can't throw on an invalid node context (tuples and some
 * generic instantiations do), falling back to the bare `getText()` and
 * finally to `unknown` so a single bad type never aborts the whole resolve.
 */
export function namedText(type: Type): string {
  try {
    return type.getText(
      undefined,
      ts.TypeFormatFlags.NoTruncation | ts.TypeFormatFlags.InTypeAlias,
    );
  } catch {
    try {
      return type.getText();
    } catch {
      return 'unknown';
    }
  }
}
