/**
 * Anchor resolution for the surface entry: guards for addressable anchors
 * (symbol / handler_return) and the locator + node-builder path for
 * anonymous inferred types. Produces the final alias line for each anchor,
 * or a recorded demotion -- never a silently wrong line.
 */
import ts from 'typescript';
import type { CaptureAnchorRequest, InferAnchorRequest } from './api.js';
import type { UnresolvedAtAnchor } from './deep-walk.js';
export interface ResolvedAnchor {
    request: CaptureAnchorRequest;
    /** RHS of `export type <alias> = ...;` in the surface entry. */
    aliasText: string;
    serialization: 'emitted' | 'node_builder' | 'structural_fallback';
    /** Present exactly for demotions (the alias line is `unknown`). */
    failureReason?: string;
    /**
     * Provenance for a non-demoting re-aim (#438/#439): recorded into the
     * alias's `self_check_detail` when the alias self-checks clean, so a symbol
     * anchor redirected from a value const to its `typeof` type sibling, or a
     * builder-chain anchor moved off its config descriptor, stays traceable.
     */
    reaimNote?: string;
    /**
     * carrick#766: why an alias ABSTAINED — the surface line is `unknown`
     * because nothing was resolvable, not because a step failed.
     *
     * Deliberately NOT `failureReason`. The scanner's literal backfill
     * (`backfill_anchors`, engine/type_compat_v2.rs) keys on
     * `capture_failure_reason` and re-anchors any alias carrying one off the v1
     * bundle's text. For an alias whose v1 answer was itself blind that text is
     * a bare element symbol, and re-anchoring it would publish a confident
     * contract whose array-ness was guessed — the #349/#306 false mismatch. An
     * abstain has nothing better to fall back to, so it must not look
     * backfillable; the reason rides `self_check_detail` instead.
     */
    abstainReason?: string;
    /**
     * carrick#1165: identifiers the alias text names (a literal anchor's text
     * or a node-builder print) that nothing in the producer's program declares.
     * Recorded on the capture record as `undeclared_names`.
     */
    undeclaredNames?: string[];
    /**
     * carrick#1164: member paths the SOURCE program could not resolve, and the
     * unresolved imports the anchor's file reaches. Printed into the surface
     * those members read `any`, like an author's `any`; the self-check uses this
     * to label them `unresolved_import` instead of `declared`. Absent when the
     * anchor's type holds no unresolved placeholder.
     */
    unresolved?: UnresolvedAtAnchor;
    /**
     * carrick#1446: set on a demotion whose alias text names a module the
     * declaration emit skipped. The text dangles in the emitted tree, which the
     * record states for a literal anchor, whose text is what the index serves.
     */
    namesUnemittedModule?: true;
}
/**
 * Repo-root-relative source file -> the specifier the surface entry imports
 * it by: its path from `entryDir`, in the form the entry resolves.
 *
 * With the extension removed (`./a`), where `resolveFromEntry` says the entry
 * resolves that to the module. It does not always (carrick#1911): under
 * `module` `node16`..`nodenext` an entry in a `"type": "module"` package is
 * an ES module, which must name the file the module compiles to, and a `.mts`
 * or `.cts` module has no extensionless form under any setting. There, and
 * with no resolver to ask, the specifier is that output name: `./a.js` for
 * `.ts` and `.tsx`, `./a.mjs` for `.mts`, `./a.cjs` for `.cts`, which every
 * module setting maps back to the source and, in the stub, to its
 * declaration.
 */
/**
 * The absolute path of an anchor's `source_file`. The scanner sends a file
 * outside the service root as an absolute path; `path.join` would nest it
 * under the root, so a source file resolves with `path.resolve`: absolute
 * stays absolute, relative is taken from the root.
 */
export declare function resolveSourceFile(repoRoot: string, sourceFile: string): string;
export declare function entryRelativeSpecifier(entryDir: string, repoRoot: string, sourceFile: string, resolveFromEntry?: (specifier: string) => string | undefined): string;
export declare function moduleExport(checker: ts.TypeChecker, sourceFile: ts.SourceFile, name: string): ts.Symbol | undefined;
/**
 * Resolve one anchor against the analysis program. `placeholder` is the
 * anchor's own placeholder alias declaration in the entry file -- the
 * destination node the node builder anchors to (banked correction 1).
 */
export declare function resolveAnchor(program: ts.Program, request: CaptureAnchorRequest, args: {
    repoRoot: string;
    entryDir: string;
    placeholder: ts.TypeAliasDeclaration | undefined;
    /**
     * symbol_name -> entry-relative specifier for every sibling `symbol`
     * anchor. A literal anchor whose text is a bare identifier naming one of
     * these resolves through that module instead of dangling in the entry.
     */
    siblingSymbolSpecs?: Map<string, string>;
    /**
     * The file a module specifier resolves to from the surface entry, under
     * the analysis program's own resolution (carrick#1789). A literal's name
     * is imported through a package specifier only when the entry reaches the
     * same module by it.
     */
    resolveFromEntry?: (specifier: string) => string | undefined;
}): ResolvedAnchor;
/**
 * Locate the target node: tightest expression covering the byte span when
 * given; else the expression matching expression_text on/after line_number;
 * else the first expression starting on line_number.
 */
export declare function locateNode(sourceFile: ts.SourceFile, request: InferAnchorRequest): ts.Node | undefined;
