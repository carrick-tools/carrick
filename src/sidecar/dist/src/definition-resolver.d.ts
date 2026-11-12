/**
 * DefinitionResolver - Resolves surface type aliases from a v2 capture stub
 * package's declaration tree using the TypeScript compiler.
 *
 * Two forms are produced per alias:
 *  - `definition`: the declaration *as written* (named refs preserved). For a
 *    surface alias `export type A = import('./m').Order;` this follows the
 *    alias to its target declaration in the stub tree (`interface Order {...}`)
 *    so the definition keeps its real name and members; when the target is
 *    anonymous (inline object types, node-builder prints) the surface alias
 *    line itself is the as-written form.
 *  - `expanded`: the fully *structural* form, with every named member type
 *    inlined to its member structure, recursively.
 *
 * An alias whose own type did not resolve produces neither: the tree is read
 * with nothing installed, so a reference that leaves it lands on TypeScript's
 * unresolved-reference placeholder, which the compiler prints as the reference
 * text rather than as `any` (see `isUnresolvedReference`). Both forms are then
 * the top type, which is what every downstream rule about an empty answer is
 * written to refuse.
 *
 * `type.getText(node, NoTruncation)` does NOT inline named members — the
 * compiler prints a referenced type by its symbol name when that symbol is in
 * scope (`total: Money`, not `total: { amountCents: number; currency: string }`).
 * The structural form is produced by `expandTypeStructural` (shared with the
 * inference path in `type-inferrer.ts`), which walks the resolved `Type` and
 * rebuilds the inlined text.
 *
 * Each resolve call builds its own throwaway project over the stub tree and
 * reads nothing else: not the service's project, and not whether the sidecar
 * was ever initialised. So the request costs what the stub costs, in a process
 * that has built nothing as in one that has (carrick#1927), and the sidecar's
 * long-lived project never sees stub files.
 */
export interface ResolvedDefinition {
    type_alias: string;
    /** Original declaration text as written (preserves named types) */
    definition: string;
    /** Fully structural form: named member types inlined to their structure */
    expanded: string;
}
export declare class DefinitionResolver {
    /**
     * Resolve surface aliases from a capture stub package directory
     * (`<stub_dir>/types/surface.d.ts` + its declaration tree).
     *
     * Uses a DEDICATED project with `moduleResolution: Bundler`, not the
     * repo's own project: the stub tree's relative import-types are
     * extensionless, which a NodeNext-configured repo project silently fails
     * to resolve (the alias then reads as `any`). Bundler resolution accepts
     * both extensionless and `.js`-suffixed specifiers — the same policy the
     * check-phase workspace uses.
     */
    resolveFromStub(stubDir: string, aliases: string[]): ResolvedDefinition[];
    /**
     * Resolve a single alias: the original text and the structural form.
     */
    private resolveAlias;
    private log;
    private logError;
}
