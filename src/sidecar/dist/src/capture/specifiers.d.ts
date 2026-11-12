/**
 * Module-specifier helpers shared across the capture bundle: extraction from
 * declaration text, external/internal classification, and tsconfig-`paths`
 * pattern matching for the post-emit rewrite pass.
 */
export declare function isRelative(spec: string): boolean;
/** A declaration file of any module format: `.d.ts`, `.d.mts`, `.d.cts`. */
export declare function isDeclarationFileName(name: string): boolean;
/**
 * The declaration files a relative specifier can name in a stub's tree, in
 * the order tried, given the path it resolves to from the file that holds it.
 *
 * A specifier names its module with no extension (`./a`: `a.d.ts`, or the
 * directory's `index.d.ts`), by the file the module compiles to (`./a.js`:
 * `a.d.ts`, `./a.mjs`: `a.d.mts`, `./a.cjs`: `a.d.cts`), or by the
 * declaration file itself. The surface names an anchor's module the second
 * way where the first does not resolve (carrick#1911), and so does any source
 * written for `node16`..`nodenext`.
 */
export declare function declarationCandidates(resolved: string): string[];
/** zod -> zod, @scope/pkg/sub -> @scope/pkg, pkg/sub -> pkg */
export declare function packageNameOf(spec: string): string;
/** Extract every module specifier mentioned in a .d.ts text: `from "x"`,
 * `import "x"`, and `import("x")` type references. */
export declare function collectSpecifiers(text: string): Set<string>;
/**
 * One parsed tsconfig `paths` mapping: pattern "prefix*suffix" (or exact) to
 * substitution targets resolved against the config's path base.
 */
export interface PathsPattern {
    /** Original pattern, e.g. "@app/*" */
    pattern: string;
    prefix: string;
    /** undefined for exact (starless) patterns */
    suffix?: string;
    /** Absolute target templates ("*" preserved), in declaration order. */
    targets: string[];
}
/**
 * Match a specifier against a pattern. Returns the "*" capture ('' for exact
 * matches) or undefined when there is no match.
 */
export declare function matchPathsPattern(spec: string, p: PathsPattern): string | undefined;
/**
 * Rewrite every specifier occurrence in a .d.ts text through `map`. The map
 * receives each specifier and returns a replacement or undefined (keep).
 * Operates on the same syntactic positions collectSpecifiers finds, so the
 * two stay consistent by construction.
 */
export declare function rewriteSpecifiers(text: string, map: (spec: string) => string | undefined): {
    text: string;
    rewrites: number;
};
