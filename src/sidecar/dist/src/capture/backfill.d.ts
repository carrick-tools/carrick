/**
 * The literal backfill (carrick#2041): which aliases a capture captures a
 * second time, and the host that lets the second surface reuse what the first
 * one parsed.
 *
 * When a capture demotes an alias (its anchor did not resolve, or its module's
 * declaration emit was skipped) and the scanner holds a usable literal type
 * text for it, the same capture writes a second stub with that alias anchored
 * on the text instead (`anchor_origin: anchor-backfill`). The scanner reads
 * both record sets and keeps the second stub only when every backfilled alias
 * self-checked clean and no sibling degraded.
 */
import ts from 'typescript';
import type { CaptureAnchorRequest } from './api.js';
import type { ResolvedAnchor } from './anchors.js';
/** The anchors of the second surface, and the aliases it re-anchors. */
export interface BackfillPlan {
    anchors: CaptureAnchorRequest[];
    /** The re-anchored aliases, in anchor order. */
    backfilled: string[];
}
/**
 * Every alias the first surface demoted (`failureReason` is set exactly for a
 * demoted anchor) for which a text is held becomes a literal anchor on that
 * text at `anchor-backfill` origin, keeping the replaced anchor's file. An
 * anchor that is already a literal is never re-anchored: the text tier is
 * the one it is on. `undefined` when no alias qualifies.
 */
export declare function backfillRerun(anchors: readonly CaptureAnchorRequest[], resolved: ReadonlyArray<Pick<ResolvedAnchor, 'request' | 'failureReason'>>, texts: Readonly<Record<string, string>> | undefined): BackfillPlan | undefined;
/**
 * `host` answering every file parsed once into `parsed` with the same source
 * file, except the files `fresh` names. Programs built on such hosts under
 * the same options share their parsed (and bound) files, as an editor's
 * programs do: the backfill's emit reuses the first emit's files, and the two
 * stubs' self-checks reuse the declarations of the installed packages.
 */
export declare function sharingHost(host: ts.CompilerHost, fresh: (fileName: string) => boolean, parsed?: Map<string, ts.SourceFile | undefined>): ts.CompilerHost;
