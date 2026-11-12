/**
 * Place every emitted declaration inside the stub's types tree
 * (carrick#1770).
 *
 * The tree mirrors the emit's rootDir. A program can also reach a source
 * outside it: a service that reads a sibling package's SOURCE through a
 * `paths` mapping or a relative import. tsc does not emit that file's
 * declaration under `outDir`; it hands the write callback the source's own
 * path with `.d.ts`. Such a file is placed under `OUTSIDE_DIR`, mirroring its
 * path from the deepest directory it shares with rootDir, and every relative
 * specifier that crosses between the two parts of the tree is rewritten to
 * where its target now sits. Specifiers are read the way tsc wrote them:
 * relative to the SOURCE location of the file that holds them.
 *
 * A capture whose program stays inside rootDir is placed exactly as before.
 */
/** Tree directory holding declarations of sources outside rootDir. */
export declare const OUTSIDE_DIR = "__outside__";
export interface PlacedTree {
    /** Emitted file name (as tsc gave it) -> tree-relative POSIX path. */
    relOf: Map<string, string>;
    /** Emitted file name -> declaration text, cross-root specifiers rewritten. */
    textOf: Map<string, string>;
    /**
     * Absolute source-side path of each outside declaration, extension
     * stripped -> its tree-relative path. Empty when nothing was outside.
     */
    outside: Map<string, string>;
    /** Relative specifiers rewritten across the root. */
    rewrites: number;
}
/**
 * Where each emitted declaration goes. `emitted` maps tsc's file name to its
 * text; `surfaceDeclaration` is renamed to `surface.d.ts` at the tree root.
 */
export declare function placeEmittedTree(args: {
    emitted: Map<string, string>;
    staging: string;
    entryDir: string;
    surfaceDeclaration: string;
}): PlacedTree;
/**
 * A test for whether the tree holds the module a specifier in the surface
 * entry names (carrick#1773).
 *
 * The specifier is read the way the placement above reads one: resolved from
 * the surface's SOURCE-side directory, so an absolute path stays what it is
 * and `../` leaves rootDir, then looked up among the source-side paths of the
 * declarations the tree holds. Those are every emitted declaration, inside
 * rootDir or placed under `OUTSIDE_DIR`, and the declaration sources shipped
 * verbatim (`declarationSources`, relative to rootDir).
 */
export declare function surfaceModuleInTree(args: {
    /** tsc's file name for each emitted declaration. */
    emitted: Iterable<string>;
    declarationSources: Iterable<string>;
    staging: string;
    entryDir: string;
    surfaceDeclaration: string;
}): (spec: string) => boolean;
