/**
 * Diagnostic parsing + bucket classification for the v2 check phase.
 *
 * The judge is the vendored `tsc` CLI with `--pretty false`, run from the
 * workspace root so file locations print workspace-relative (no temp path in
 * the location prefix). This module turns that text into per-pair verdicts,
 * classifying by diagnostic code + file + line (never by line position alone):
 *
 *   poison (stub-file diagnostic)         -> unverifiable   [highest precedence]
 *   surface import error (probe lines 1-2)-> unverifiable
 *   IsAny gate fired (TS2344)             -> gate_caught_baked_any
 *   IsUnknown/IsNever gate fired (TS2344) -> unverifiable
 *   IsVoid gate fired (TS2344)            -> compatible with a note when the
 *                                            consumer reads no response body;
 *                                            else unverifiable (no body read)
 *   IsFormBody gate fired (TS2344)        -> unverifiable (form-encoded body)
 *   IsByteBody gate fired (TS2344)        -> unverifiable (bytes, even agreeing)
 *   raw-text read marked on a side        -> unverifiable (text, even agreeing)
 *   assignment error, sent union taken in part by a weak expected type
 *                                         -> unverifiable (carrick#1995)
 *   assignment error on the published type that the sent side's unwidened
 *   reading does not repeat               -> producer_wider (carrick#1538),
 *                                            or compatible with a note when
 *                                            the consumer sends (carrick#2011)
 *   assignment-class error                -> incompatible
 *   no diagnostics                        -> compatible     [lowest precedence]
 *
 * Gate precedence over the assignment line is load-bearing: an `unknown` side
 * produces BOTH a gate TS2344 and an assignment TS2322, and reading the latter
 * would mislabel an unverifiable pair as incompatible.
 *
 * Seam: node builtins + this bundle only.
 */
import type { CheckVerdict } from './api.js';
import { type ProbePlan, type Side } from './check-probe.js';
import { type ScrubContext } from './check-scrub.js';
import type { PairDeepFindings } from './check-deep.js';
import type { DispatchUnion } from './check-union.js';
import { type PairFieldReport } from './check-fields.js';
export interface RawDiagnostic {
    /** Workspace-relative, forward-slash file path (empty for global errors). */
    file: string;
    line: number;
    col: number;
    code: number;
    /** Primary text plus any indented elaboration lines, joined with '\n'. */
    message: string;
}
/** Parse `tsc --pretty false` output into structured diagnostics. */
export declare function parseTscOutput(stdout: string): RawDiagnostic[];
export interface ClassifyInput {
    plan: ProbePlan;
    /** Diagnostics attributed to this pair's probe file. */
    probeDiags: RawDiagnostic[];
    /**
     * Returns a reason string when THIS alias of the service is poisoned (#438
     * part 2: poison is contained to the aliases whose closure includes the
     * poisoned file, not the whole service).
     */
    poisonReason: (serviceName: string, alias: string) => string | undefined;
    scrubCtx: ScrubContext;
    /**
     * Deep any/unknown findings for this pair's two sides, walked in the
     * assembled workspace after the pinned externals installed (carrick#707,
     * R1d). `undefined` when the walk could not run or could not resolve the
     * aliases -- absence of findings is not evidence of cleanliness, so the
     * verdict is then not a fact either.
     */
    deepFindings?: PairDeepFindings;
    /**
     * The differing fields of the two types the judge compared
     * (carrick-tools/carrick-cloud#1118), walked in the same program. Read only
     * on a mismatch, and only to NAME what the judge already decided: it never
     * moves a bucket and never sets `resolved`. Absent when the walk could not
     * run, in which case the tsc text stands alone.
     */
    fieldReport?: PairFieldReport;
    /**
     * Set when the sent side is a union the expected side takes only in part,
     * every other member failing by nothing but the compiler's weak-type check
     * (carrick#1995). Read only where the decisive assignment failed.
     */
    dispatchUnion?: DispatchUnion;
    /**
     * The side whose capture record says it reads the body as raw text
     * (carrick#1842), the sent side when both do. Read from the record, not the
     * type: a text read publishes `string`, which no type gate can tell from a
     * JSON body that is a string.
     */
    rawTextSide?: Side;
}
/** Classify one pair into exactly one bucket, honouring the precedence order. */
export declare function classifyPair(input: ClassifyInput): CheckVerdict;
/**
 * The note on a request the consumer's unwidened reading cleared
 * (carrick#2011).
 */
export declare const CONSUMER_WIDER_NOTE: string;
/** The note on a pair whose consumer reads no response body (carrick#1162). */
export declare const CONSUMER_READS_NO_BODY_NOTE = "the consumer reads no response body (its expected response is void), so the producer response is not compared with a read";
