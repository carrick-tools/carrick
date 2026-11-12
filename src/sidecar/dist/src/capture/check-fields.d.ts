/**
 * Field-level report for a pair the judge called incompatible
 * (carrick-tools/carrick-cloud#1118).
 *
 * `tsc` decides. Its elaboration names ONE field and then elides the rest
 * ("Type 'A' is not assignable to type 'B'. Property 'x' is missing"), which is
 * not enough for a reader deciding what to change: a create endpoint that
 * requires `username` while the client sends `userName` reads as one missing
 * property with no hint that the sent object carries a near-neighbour.
 *
 * This walk enumerates the differing fields of the SAME two types the judge
 * compared, in the SAME program, using the compiler's own assignability
 * relation. It is not a second judge:
 *   - it runs only after the bucket is decided and never changes one;
 *   - it never contradicts: a difference is only named when the checker itself
 *     says the two member types do not assign, and when it finds nothing it
 *     adds nothing and the raw tsc text stands alone;
 *   - it refuses the shapes where a member list is not an account of the type
 *     (a union root, a receiver with an index signature), rather than guessing
 *     about them.
 *
 * It reports one thing the judge structurally cannot: an optionality gap in the
 * direction that still assigns (the sending side always provides a field the
 * receiving side declares optional). That is a real drift between two sources
 * — the receiver carries a branch that never runs — and no assignment error can
 * exist for it. It is stated as an observation beside the verdict, never as the
 * verdict.
 *
 * Seam: node builtins + `typescript` + this bundle only.
 */
import ts from 'typescript';
import type { ProbePlan, Side } from './check-probe.js';
import type { ProbeProgram } from './check-deep.js';
/** What kind of difference one field path carries. */
export type FieldDifferenceNature = 
/** The receiving side declares it; the sending side has no such member. */
'missing_in_sent'
/** The sending side provides it; the receiving side declares no such member. */
 | 'extra_in_sent'
/** Optional where it is sent, required where it is read. */
 | 'optional_in_sent'
/** Always sent, optional where it is read (no assignment error can exist). */
 | 'optional_in_expected'
/** Both declare it and the member types do not assign. */
 | 'type_differs';
export interface FieldDifference {
    /** Dotted member path from the compared root (`''` is the root itself). */
    path: string;
    nature: FieldDifferenceNature;
    /** Printed member type on the sending side, for `type_differs`. */
    sentText?: string;
    /** Printed member type on the receiving side, for `type_differs`. */
    expectedText?: string;
}
export interface PairFieldReport {
    /** Named differences, capped and ordered deterministically. */
    differences: FieldDifference[];
    /** How many further differences were found beyond the cap. */
    truncated: number;
    /**
     * Whether the sent type's JSON wire form differs from its declared form, so
     * the comparison the reader is being shown is against the serialised shape
     * (a `Date` compared as the string it serialises to).
     */
    wireApplied: boolean;
}
/**
 * Cap on named fields. A mismatch with more differing members than this is
 * better described as two unrelated shapes than as a list, and the text says
 * how many more there are rather than pretending the list is complete.
 */
export declare const MAX_NAMED_FIELDS = 8;
/**
 * The two types the judge's decisive assignment compared for one plan, read in
 * the probe program, with the compiler's own assignability relation.
 * `undefined` when the probe or either alias cannot be read, or the compiler
 * build exposes no relation: every statement made without them would be a
 * guess.
 *
 * The sent side is whatever that assignment actually sent: the GraphQL
 * comparand where one exists, then the JSON wire form where serialising
 * changes the type. Reading `sent` there instead would describe a type the
 * judge did not compare, which is the one way a reader of this pair could
 * contradict it.
 */
export declare function comparedTypes(opened: ProbeProgram, plan: ProbePlan): {
    compared: ts.Type;
    expected: {
        type: ts.Type;
        node: ts.Node;
    };
    wireApplied: boolean;
    checker: ts.TypeChecker;
    isAssignableTo: (source: ts.Type, target: ts.Type) => boolean;
} | undefined;
/**
 * Field reports for every plan whose probe the program could read, keyed by
 * pair id. A plan with no entry has no report, which is not a claim that its
 * types agree.
 */
export declare function pairFieldReports(opened: ProbeProgram | undefined, plans: ProbePlan[]): Map<string, PairFieldReport>;
/**
 * The one wording for "this comparison was made against the serialised form",
 * shared by the mismatch diagnostic and the `notes` channel so the two can
 * never drift apart (carrick#1341).
 */
export declare function wireFormNote(sentSide: Side): string;
/**
 * The statements this report makes that are NOT a mismatch: what belongs on
 * `CheckVerdict.notes` (carrick#1341).
 *
 * Two of the things the walk can find are true of a pair the judge called
 * COMPATIBLE, and so have no mismatch diagnostic to ride on:
 *
 *  - the wire note. Since carrick#1340 a producer `Date` read as a `string` is
 *    not a drift, because that is what arrives. A reader comparing the two
 *    declared shapes by hand sees `Date` against `string` and concludes the
 *    check missed it, so the verdict has to say the comparison was made
 *    against the serialised form.
 *  - an optionality gap (`optional_in_expected`): the sending side always
 *    provides a field the receiving side declares optional. That assigns, so
 *    no diagnostic can exist for it, and the two sources still disagree — the
 *    receiver carries a branch that never runs.
 *
 * Both are OBSERVATIONS. Nothing here is a verdict, nothing here may move one,
 * and this is never a substitute for a mismatch reason: a caller that finds
 * notes on an incompatible row has one statement made twice, not two
 * statements. Returned in the report's own deterministic order.
 */
export declare function fieldReportNotes(report: PairFieldReport, sentSide: Side, expectedSide: Side): string[];
/**
 * The sentence appended to a mismatch diagnostic. Names the two sides as
 * producer and consumer (never the probe's internal sent/expected), so the
 * reader knows which repo to change.
 */
export declare function describeFieldReport(report: PairFieldReport, sentSide: Side, expectedSide: Side): string;
