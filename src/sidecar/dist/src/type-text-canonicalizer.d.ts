/**
 * Canonical union order inside text the COMPILER printed (carrick#775, #760).
 *
 * `expandTypeStructural` walks a type and orders every union it walks
 * (`orderMembers`), so the structural path is stable. But the walk stops at
 * several points and hands the rest of the subtree to `namedText` — the
 * compiler's own print:
 *
 *  - library types, which are kept by name (`isLibraryType`), and whose print
 *    carries their whole generic instantiation with it;
 *  - object types with no named properties (an index signature alone), which
 *    have nothing to walk;
 *  - tuples, functions, cycles the `seen` set breaks, and the depth backstop.
 *
 * Everything inside one of those prints is in the compiler's own union order,
 * which is type-id order, which is CREATION order — the same instability
 * `orderMembers` exists to remove, one level down where the type API can no
 * longer reach it: at that point the members exist only as text.
 *
 * So they are ordered as text, by the same rule the walk uses: every member
 * sorts by its own rendering, compared by UTF-16 code unit. A union is a set,
 * the check phase compares these strings by typechecking them, and a reader
 * diffing two `expanded_definition` strings cannot tell which side of a
 * `namedText` boundary a member came from — so both sides must order them the
 * same way, which is why `orderMembers` no longer keeps intrinsics in compiler
 * id order (the compiler's printer does not use that order either).
 *
 * The rewrite is deliberately timid. Anything whose meaning could depend on
 * order or on a parse this file does not do — a function type (`=>`), a
 * conditional (`?`) — is returned untouched, and untouched is always a legal
 * answer here because the compiler's print is already valid TypeScript.
 */
/**
 * Put every union in `text` into canonical order, at every nesting level.
 *
 * Returns `text` unchanged whenever the shape is one this file does not parse
 * (see the module comment). Never throws: a type print is arbitrary text, and
 * a definition resolve must not fail because one member confused a scanner.
 */
export declare function canonicalizeUnionsInText(text: string): string;
/**
 * A union's rendered members with the literal pair `false`, `true` folded into
 * `boolean` (carrick#1165).
 *
 * The compiler stores `boolean` as exactly that pair, and its own printer folds
 * it back. Anything that renders a union member by member — the structural
 * walk, and this file's reordering of a compiler print — sees the two literals
 * separately, and sorting them apart produced `false | null | true` for
 * `boolean | null`. Folding before the sort gives the one print a reader and a
 * diff expect. A lone literal is a literal type and is kept. Shared by
 * `orderMembers` in `type-structural-expander.ts`, so both halves of the normal
 * form fold the same way.
 */
export declare function foldBooleanLiterals(members: string[]): string[];
