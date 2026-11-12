/**
 * TypeBundler - the `bundle` action: each requested symbol's declaration text,
 * read from the init'd ts-morph project.
 */
import { Project, SourceFile } from 'ts-morph';
import type { SymbolRequest, BundleResult } from './types.js';
import { type ExpandOrigin } from './type-structural-expander.js';
/** Where a requested symbol is declared: the file and the name it has there. */
interface DeclarationSite {
    sourceFile: SourceFile;
    name: string;
}
/**
 * Where `name`, as `sourceFile` exports it, is declared (carrick#1605). A name
 * the file declares is its own. Otherwise it is followed the way an import of
 * it is: a model names a type where the consumer imports it from, usually a
 * barrel that only has `export *`.
 */
export declare function declarationSite(sourceFile: SourceFile, name: string, label: string, origin: ExpandOrigin): DeclarationSite | {
    reason: string;
};
/**
 * Options for TypeBundler construction (legacy)
 */
export interface TypeBundlerOptions {
    /** The ts-morph Project instance */
    project: Project;
    /** The repository root directory (absolute path) */
    repoRoot: string;
}
/**
 * TypeBundler - Legacy bundler using simpler approach
 *
 * This is a simplified replacement that doesn't use dts-bundle-generator.
 * It extracts type definitions directly using ts-morph.
 */
export declare class TypeBundler {
    private readonly project;
    private readonly repoRoot;
    constructor(options: TypeBundlerOptions);
    /**
     * Bundle the requested symbols into type definitions
     *
     * @param symbols - Array of symbol requests to bundle
     * @returns BundleResult with the bundled content or errors
     */
    bundle(symbols: SymbolRequest[]): BundleResult;
    private dedupeSymbols;
    private validateSymbols;
    private validateSymbol;
    /** Where a requested symbol is declared, or undefined when it cannot be found. */
    private siteOf;
    /**
     * Structural body (`{ ... }`) for an interface/object type, with every nested
     * named member inlined recursively. Returns null when the expansion is not a
     * usable object body (e.g. the expander fell back to a bare name), so the
     * caller can keep the verbatim declaration instead of emitting an invalid
     * interface. The expander already guards recursion (depth + per-branch cycle
     * set) and keeps builtins/`node_modules` types by name.
     */
    private expandObjectBody;
    /**
     * The program and service root the printer decides library origin from; see
     * `ExpandOrigin`.
     */
    private expandOrigin;
    private extractTypeDefinition;
    /**
     * carrick#1841: what a consumer receives when the symbol the model named for
     * its response is a response table keyed by status code (`{ 200: Item;
     * 404: Problem }`): the table's 2xx values, expanded like any other body.
     * `unknown` where the table states no success body (no 2xx row, or only
     * rows such as `204: void`). `undefined` when the symbol is not a table, so
     * the caller bundles it as written. The capture's symbol anchor reads the
     * same rule (`capture/anchors.ts`), so both answers agree.
     */
    private statusTableBodyText;
    private elementNeedsArrayParens;
    private extractTypeDefinitionBase;
    private isPathSource;
    private isPlaceholderSource;
    private log;
    private logError;
}
export {};
