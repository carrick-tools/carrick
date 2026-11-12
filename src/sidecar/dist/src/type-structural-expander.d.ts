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
import { type Node, type Type, ts } from 'ts-morph';
import { type ExternalImports } from './origin.js';
/**
 * Bound on the structural-expansion recursion. Deep enough for every realistic
 * request/response shape; a backstop against pathological/recursive types the
 * cycle set somehow misses.
 */
export declare const MAX_EXPANSION_DEPTH = 12;
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
 * cannot disagree about which types to inline. `imports` is that verdict kept
 * past the program rebuilds that drop it (carrick#1731), for a project the
 * loader built.
 */
export interface ExpandOrigin {
    readonly program: ts.Program;
    readonly repoRoot: string;
    readonly imports?: ExternalImports;
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
export declare function expandTypeStructural(type: Type, origin: ExpandOrigin, options?: ExpandOptions): string;
/**
 * The type `JSON.stringify` serialises in place of a value of `type`: the
 * return type of the value's own `toJSON()`, or `undefined` when it declares
 * none (carrick#1163).
 *
 * Only a callable `toJSON` member counts, and only a return type that says
 * something: an `any` return describes nothing, and a `toJSON` that returns
 * its own type again maps nothing either.
 */
export declare function jsonWireType(type: Type): Type | undefined;
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
export declare function namedText(type: Type): string;
