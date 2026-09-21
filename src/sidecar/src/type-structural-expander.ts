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
 * (carrick#735), which the depth backstop applies too, and which `namedText`
 * applies as text to everything the walk hands back to the compiler's own
 * print (carrick#775).
 *
 * Shared by `definition-resolver.ts` (bundle alias resolution) and
 * `type-inferrer.ts` (consumer-side inference), so both paths emit the same
 * structural form rather than a dangling name.
 */

import { type Node, type Symbol, type Type, ts } from 'ts-morph';
import {
  canonicalizeUnionsInText,
  foldBooleanLiterals,
} from './type-text-canonicalizer.js';
import { isExternalOrigin } from './origin.js';

/**
 * Bound on the structural-expansion recursion. Deep enough for every realistic
 * request/response shape; a backstop against pathological/recursive types the
 * cycle set somehow misses.
 */
export const MAX_EXPANSION_DEPTH = 12;

/**
 * Types to print at named member positions instead of the walked type
 * (carrick#1105).
 *
 * A validation schema's request contract is its INPUT, except at a member
 * whose input is `unknown` and whose output is concrete (a coercion): that
 * member prints the OUTPUT type. The substitution is per member, so it has to
 * happen inside the walk, where the member is printed.
 *
 * Positions use the member-path notation of the provenance entries: `''` for
 * the root, `sub.field` for a property (keyed by symbol name), `items<0>` for
 * an array element; union and intersection members share their parent's
 * position. The walk records every position it printed an override at in
 * `applied`, so a caller can tell a substitution the walk could not reach (a
 * tuple, a cycle, the depth backstop) from one it made.
 *
 * `at` is the node the member types are read at while walking toward an
 * override. A library's inferred object type is a mapped type whose member
 * declaration sits inside a generic, where the member reads as `any`; read at
 * the schema's own node it is the instantiated type, the same read the
 * caller's position walk makes.
 */
export interface MemberOverrides {
  readonly types: ReadonlyMap<string, Type>;
  readonly applied: Set<string>;
  readonly at: Node;
}

/** The overrides narrowed to one position of the walk. */
interface OverrideCursor {
  readonly overrides: MemberOverrides;
  readonly position: string;
}

/** True when some override sits strictly below `position`. */
function hasOverrideBelow({ overrides, position }: OverrideCursor): boolean {
  for (const key of overrides.types.keys()) {
    if (
      position === ''
        ? key !== ''
        : key.startsWith(`${position}.`) || key.startsWith(`${position}<`)
    ) {
      return true;
    }
  }
  return false;
}

function childCursor(
  cursor: OverrideCursor | undefined,
  segment: string,
): OverrideCursor | undefined {
  if (!cursor) return undefined;
  const position = segment.startsWith('<')
    ? `${cursor.position}${segment}`
    : cursor.position === ''
      ? segment
      : `${cursor.position}.${segment}`;
  return { overrides: cursor.overrides, position };
}

/**
 * The representation a type is printed in.
 *
 * `'declared'` prints the type the source declares. `'json'` prints what a
 * JSON serialiser puts on the wire for a value of that type (carrick#1163):
 * `JSON.stringify` calls a value's `toJSON()` and serialises its RESULT, so a
 * `Date` travels as the string its `toJSON` returns, and so does any other
 * type that declares one. The mapping is read from the compiler's own
 * signature, never from a list of type names.
 */
export type WireFormat = 'declared' | 'json';

/**
 * The program the walked type belongs to, and the service root inside it.
 *
 * Required, not optional, because the walk cannot decide from a type alone
 * whether a declaration is the user's source or something the runtime
 * installed: a path test answers that only where resolution goes through
 * `node_modules`, and a runtime that serves an npm dependency's types out of
 * its own cache leaves no such segment in the path (carrick#1264). The program
 * carries the resolver's own verdict, so it is what `isExternalOrigin` is
 * asked — the same instrument the inference path uses, so the two layers
 * cannot disagree about which types to inline.
 */
export interface ExpandOrigin {
  readonly program: ts.Program;
  readonly repoRoot: string;
}

/** Everything `expandTypeStructural` takes besides the type and its origin. */
export interface ExpandOptions {
  /** Substitutions at named member positions; see `MemberOverrides`. */
  readonly overrides?: MemberOverrides;
  /** Which representation to print; see `WireFormat`. */
  readonly wire?: WireFormat;
  /**
   * Where in the recursion the walk starts. Production callers never set it —
   * depth is walk state — but the `MAX_EXPANSION_DEPTH` backstop is reachable
   * from a shallow type only by starting the walk at the bound, which is how
   * its union ordering is tested.
   */
  readonly depth?: number;
}

/**
 * Recursively render a `Type` as fully-inlined structural text.
 *
 * Named object/interface types are expanded to their member structure;
 * primitives, literals, library types (`Date`, `Promise`, tuples, …) and
 * functions stay by name — `origin` is what decides which is which. A cycle
 * set (object type ids on the current branch) breaks reference cycles and
 * `MAX_EXPANSION_DEPTH` is a hard backstop; both are walk state, not caller
 * state. `overrides` substitutes a type at named member positions
 * (`MemberOverrides`); without it the print is unchanged. `wire` picks the
 * representation (`WireFormat`).
 */
export function expandTypeStructural(
  type: Type,
  origin: ExpandOrigin,
  options: ExpandOptions = {},
): string {
  const { overrides, wire = 'declared', depth = 0 } = options;
  return expandAt(
    type,
    new Set(),
    depth,
    overrides ? { overrides, position: '' } : undefined,
    wire,
    origin,
  );
}

/**
 * The type `JSON.stringify` serialises in place of a value of `type`: the
 * return type of the value's own `toJSON()`, or `undefined` when it declares
 * none (carrick#1163).
 *
 * Only a callable `toJSON` member counts, and only a return type that says
 * something: an `any` return describes nothing, and a `toJSON` that returns
 * its own type again maps nothing either.
 */
export function jsonWireType(type: Type): Type | undefined {
  if (!type.isObject() || type.isArray() || isTuple(type)) return undefined;
  const member = type.getProperty('toJSON');
  if (!member) return undefined;
  const declaration = member.getValueDeclaration() ?? member.getDeclarations()[0];
  const memberType = declaration
    ? member.getTypeAtLocation(declaration)
    : memberTypeWithoutDeclaration(member, type);
  const signature = memberType.getCallSignatures()[0];
  if (!signature) return undefined;
  const serialised = signature.getReturnType();
  if (serialised.isAny() || serialised.isUnknown()) return undefined;
  const id = (type.compilerType as { id?: number }).id;
  const serialisedId = (serialised.compilerType as { id?: number }).id;
  if (id != null && id === serialisedId) return undefined;
  return serialised;
}

function expandAt(
  type: Type,
  seen: Set<number>,
  depth: number,
  at: OverrideCursor | undefined,
  wire: WireFormat,
  origin: ExpandOrigin,
): string {
  if (depth > MAX_EXPANSION_DEPTH) return backstopText(type);

  let cursor = at;
  if (cursor) {
    const replacement = cursor.overrides.types.get(cursor.position);
    if (replacement) {
      cursor.overrides.applied.add(cursor.position);
      return expandAt(replacement, seen, depth, undefined, wire, origin);
    }
    // Nothing to substitute below here: print exactly as without overrides.
    if (!hasOverrideBelow(cursor)) cursor = undefined;
  }

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
    return canonicalMembers(
      type.getUnionTypes(),
      seen,
      depth,
      cursor,
      wire,
      origin,
    ).join(' | ');
  }
  if (type.isIntersection()) {
    return canonicalMembers(
      type.getIntersectionTypes(),
      seen,
      depth,
      cursor,
      wire,
      origin,
    ).join(' & ');
  }

  // Tuples are array-like but must keep their `[a, b]` shape, not be walked
  // as objects (which explodes into `Array.prototype`). Handle before arrays.
  if (isTuple(type)) {
    return namedText(type);
  }

  if (type.isArray()) {
    const element = type.getArrayElementType();
    if (!element) return namedText(type);
    const inner = expandAt(
      element,
      seen,
      depth + 1,
      childCursor(cursor, '<0>'),
      wire,
      origin,
    );
    // Parenthesise a union/intersection element so `(A | B)[]` doesn't misparse
    // as `A | B[]`. Decide from the TYPE, not the string: a single object
    // literal like `{ a: A | B }` is NOT a union and must not be parenthesised,
    // and a union led by an object literal (`{ a: string } | null`) MUST be.
    const needsParens = element.isUnion() || element.isIntersection();
    return needsParens ? `(${inner})[]` : `${inner}[]`;
  }

  // On the JSON wire a value with `toJSON()` is its serialised form, library
  // type or not, so this runs before the by-name bail-out below.
  if (wire === 'json') {
    const serialised = jsonWireType(type);
    if (serialised) return expandAt(serialised, seen, depth + 1, undefined, wire, origin);
  }

  // Library / built-in types (Date, Promise, RegExp, …): keep by name, unless
  // a member below has to be substituted — a schema library declares its
  // inferred object types itself, and they are only walkable, not by-name.
  if (!cursor && isLibraryType(type, origin)) {
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

    const parts = props.map((prop) =>
      expandProperty(prop, type, nextSeen, depth, cursor, wire, origin),
    );
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
 * one — every member sorts by its own rendered text, compared by UTF-16 code
 * unit. That is a pure function of the members, with no dependence on when the
 * checker happened to create any of them.
 *
 * #735 carved out an exception for the intrinsics (`string`, `null`, `number`,
 * …), keeping them ahead of the rest in compiler-id order on the grounds that
 * their ids are fixed before any source file is read, so `string | null` would
 * still print the way the compiler prints it. The second half of that is not
 * true: the compiler's printer does not use id order. On a union of
 * `null | number` the checker's ids give `null` first and `typeToString` prints
 * `number | null`, so keeping id order reproduced neither the compiler's print
 * nor — once carrick#775 put the compiler's own prints under the same rule —
 * the other path's. One rule for every member is the only way a union prints
 * one way wherever it is rendered, and it costs nothing the exception was
 * actually buying. `type-text-canonicalizer.ts` applies the same rule to text.
 *
 * Ties can only happen between two members that render identically, in which
 * case the joined output is the same whichever way round they go.
 */
function canonicalMembers(
  members: Type[],
  seen: Set<number>,
  depth: number,
  cursor: OverrideCursor | undefined,
  wire: WireFormat,
  origin: ExpandOrigin,
): string[] {
  return orderMembers(members, (member) =>
    expandAt(member, seen, depth + 1, cursor, wire, origin),
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
  const rendered = foldBooleanLiterals(members.map(render)).map((text, index) => ({
    index,
    text,
  }));
  rendered.sort((a, b) => {
    if (a.text !== b.text) return a.text < b.text ? -1 : 1;
    return a.index - b.index;
  });
  return rendered.map((entry) => entry.text);
}

/** Render a single property as `name[?]: <expanded>`. */
function expandProperty(
  prop: Symbol,
  owner: Type,
  seen: Set<number>,
  depth: number,
  at: OverrideCursor | undefined,
  wire: WireFormat,
  origin: ExpandOrigin,
): string {
  const optional = (prop.getFlags() & ts.SymbolFlags.Optional) !== 0;

  const propDecl = prop.getDeclarations()[0];
  // Render the key from the declaration's name node so quoted/computed keys
  // ('x-y', "x y", [Symbol.iterator]) survive as valid TS text rather than
  // being unquoted into invalid output; fall back to the bare symbol name.
  const name = renderPropertyName(prop, propDecl);
  let propType = at
    ? prop.getTypeAtLocation(at.overrides.at)
    : propDecl
      ? prop.getTypeAtLocation(propDecl)
      : memberTypeWithoutDeclaration(prop, owner);

  // A substituted member takes the override's TYPE but keeps this key's
  // optionality, and is looked up before the `undefined` strip below so an
  // override that carries `| undefined` strips like any other optional key.
  let cursor = childCursor(at, prop.getName());
  const replacement = cursor && cursor.overrides.types.get(cursor.position);
  if (cursor && replacement) {
    cursor.overrides.applied.add(cursor.position);
    propType = replacement;
    cursor = undefined;
  }

  // An optional property's type includes `undefined`; the structural label
  // drops it (`note?: string`, not `note?: string | undefined`).
  if (optional && propType.isUnion()) {
    const nonUndefined = propType
      .getUnionTypes()
      .filter((member) => !member.isUndefined());
    if (nonUndefined.length === 1) {
      propType = nonUndefined[0];
    } else if (nonUndefined.length > 1) {
      const inner = canonicalMembers(
        nonUndefined,
        seen,
        depth,
        cursor,
        wire,
        origin,
      ).join(' | ');
      return `${name}?: ${inner}`;
    }
  }

  const inner = expandAt(propType, seen, depth + 1, cursor, wire, origin);
  return `${name}${optional ? '?' : ''}: ${inner}`;
}

/**
 * The type of a member the CHECKER synthesised, which has no declaration of
 * its own to be read at (carrick#1433).
 *
 * A mapped type's members — what a query builder's projection, a
 * `GetPayload<…>`-style generic or any homomorphic mapping produces — carry no
 * declaration node. `Symbol.getDeclaredType()` answers `any` for such a symbol
 * (it is the DECLARED type of a type symbol, and a value member declares
 * none), so the printed contract lost every field the compiler had resolved:
 * `{ id: string; createdAt: Date }` printed as `{ id: any; createdAt: any }`
 * and the row was demoted for carrying a top type.
 *
 * The member is read at the owning type's own declaration instead — the mapped
 * type node the checker instantiated. A synthesised member's type does not
 * depend on the location it is read at (only narrowing and `this` do, and it
 * has neither), so this is the instantiated member type; it is the same answer
 * the caller's own node gives. With no declaration anywhere to read at, the
 * declared type is still the only thing left to ask for.
 */
function memberTypeWithoutDeclaration(prop: Symbol, owner: Type): Type {
  const ownerDecl = (owner.getSymbol() ?? owner.getAliasSymbol())?.getDeclarations()?.[0];
  return ownerDecl ? prop.getTypeAtLocation(ownerDecl) : prop.getDeclaredType();
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
 * True for types the runtime or an installed package declares (Date, Promise,
 * RegExp, a framework's own types, …). These stay by name rather than being
 * inlined.
 *
 * Asked of the program (`isExternalOrigin`), not of the path: where resolution
 * does not go through `node_modules` — a runtime serving an npm dependency's
 * types from its own cache — a path test recognises nothing, and the walk
 * inlines a library's internals as if they were the user's contract. An
 * interface that extends `Array<T>` then prints as the whole array prototype,
 * whose signatures carry `thisArg?: any`, and the row is demoted for a top
 * type that is not in the contract at all (carrick#1264).
 */
function isLibraryType(type: Type, origin: ExpandOrigin): boolean {
  const symbol = type.getSymbol() ?? type.getAliasSymbol();
  if (!symbol) return false;
  const decls = symbol.getDeclarations();
  if (decls.length === 0) return false;
  return decls.some((decl) =>
    isExternalOrigin(origin.program, decl.getSourceFile().compilerNode, origin.repoRoot),
  );
}

/**
 * Non-expanded text for a type. Passes `undefined` as the enclosing node so
 * the compiler can't throw on an invalid node context (tuples and some
 * generic instantiations do), falling back to the bare `getText()` and
 * finally to `unknown` so a single bad type never aborts the whole resolve.
 *
 * This is the ONE place the walk hands a subtree back to the compiler's own
 * print — for a library type, a type with no properties to walk, a tuple, a
 * function, a cycle, or the depth backstop. Everything inside that print is in
 * type-id order, which is creation order, so the unions it contains are put in
 * the same canonical order the walk gives the ones it renders itself
 * (carrick#775). Doing it here rather than at each caller means no bail-out
 * path can print a union one way while the walk prints it another.
 */
export function namedText(type: Type): string {
  return canonicalizeUnionsInText(compilerText(type));
}

/** The compiler's print, with the two fallbacks. */
function compilerText(type: Type): string {
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
