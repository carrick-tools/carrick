/**
 * SymbolTracker-backed node-builder printing for anonymous inferred types
 * (design doc, Capture step 5). This is WP1's risk concentration, built
 * failure-visibility-first: any tracked symbol that is not plainly
 * accessible from the destination file demotes the alias to the
 * structural_fallback tier with a recorded reason -- the bundle never ships
 * a silently wrong .d.ts.
 *
 * The four banked corrections from the 2026-07-02 derisk sweep, applied
 * verbatim:
 *  1. enclosingDeclaration anchors in the DESTINATION surface file (the
 *     entry's placeholder alias for this anchor), never undefined.
 *  2. Demotion triggers on accessibility !== Accessible -- that includes
 *     CannotBeNamed (2), not just NotAccessible (1).
 *  3. Detection drives off the trackSymbol callback; the
 *     reportInaccessibleUniqueSymbolError / reportInaccessibleThisError
 *     callbacks never fire for these shapes and are implemented only as
 *     belt-and-braces recorders.
 *  4. The tracker is passed as the 5th argument of the (internal)
 *     typeToTypeNode signature: (type, enclosingDeclaration, flags,
 *     internalFlags, tracker).
 */
import ts from 'typescript';
export interface NodeBuilderPrintResult {
    /** Printed type text, present only when the print is trusted. */
    text?: string;
    /** Symbols the tracker flagged as not accessible from the destination. */
    inaccessible: string[];
    /** Failure description when text is absent. */
    failure?: string;
    /**
     * Names the print refers to that do not resolve at the destination in the
     * producer's program (carrick#1165). Present only when there are some —
     * which, since carrick#1377 rewrites what it can reach, means a reference
     * the substitution could not replace.
     */
    undeclaredNames?: string[];
    /**
     * Member positions the print named something undeclared at, and where that
     * reference now reads `unknown` (carrick#1377). In the walk's own path
     * notation, so a finding at one of them can be labelled as what it is: a
     * module that did not resolve, not a declared top type.
     */
    substitutedPaths?: string[];
}
/** One reference replaced by `unknown`, with the member position it sat at. */
export interface UndeclaredSubstitution {
    name: string;
    path: string;
}
export interface InternalTypeChecker extends ts.TypeChecker {
    isSymbolAccessible(symbol: ts.Symbol, enclosingDeclaration: ts.Node | undefined, meaning: ts.SymbolFlags, shouldComputeAliasesToMakeVisible: boolean): {
        accessibility: number;
    };
    typeToTypeNode(type: ts.Type, enclosingDeclaration: ts.Node | undefined, flags: ts.NodeBuilderFlags | undefined, internalFlags?: number, tracker?: unknown): ts.TypeNode | undefined;
}
/**
 * Print `type` as a type node anchored at `destination` (a declaration inside
 * the surface entry file). Returns untrusted-failure instead of text whenever
 * any referenced symbol is not plainly accessible from the destination.
 */
export declare function printTypeForDestination(program: ts.Program, type: ts.Type, destination: ts.Node): NodeBuilderPrintResult;
/**
 * Rewrite every reference in `node` that nothing in the producer's program
 * declares to the `unknown` keyword, and report the member position each sat
 * at (carrick#1377).
 *
 * The positions use the same notation as the capture's deep walk — `sub`,
 * `items<0>.meta`, `[index]`, `()` for a callable return — so a path found
 * here names the same member a self-check finding at that path names.
 */
export declare function substituteUndeclaredNames(node: ts.TypeNode, program: ts.Program, destination: ts.Node): {
    node: ts.TypeNode;
    substitutions: UndeclaredSubstitution[];
};
/** The deep walk's path for `child` inside `parent`, extending `path`. */
export declare function childPath(parent: ts.Node, child: ts.Node, path: string): string;
/**
 * Bare names in a printed type node that nothing in the producer's program
 * declares (carrick#1165).
 *
 * The builder names an out-of-scope declaration through `import("...")`, and
 * the tracker demotes a symbol it cannot reach, so a bare reference is meant
 * to be in scope where the alias is declared. The one way it is not: the
 * builder REUSES a source annotation as written (`parcel: Row`) when the
 * annotation's import did not resolve. There is then no symbol to track, and
 * the surface names an identifier nothing declares. Literal anchor text, which
 * the v1 walk printed, can name a type the same way.
 *
 * A name is listed only when it neither resolves at the destination (every
 * lib, runtime and `@types` global does) nor has a declaration anywhere in the
 * program. The second test matters on a healthy checkout: the v1 walk prints
 * an enum member (`Status.Open`) or a recursive reference by name, and that
 * name is declared in the project though not in scope at the surface. An
 * import binding is not a declaration, so a name whose only source is an
 * import that did not resolve is still listed. Type parameters the print
 * itself declares (generic signatures, mapped and `infer` types) are excluded.
 */
export declare function undeclaredNamesIn(node: ts.TypeNode, program: ts.Program, destination: ts.Node): string[];
