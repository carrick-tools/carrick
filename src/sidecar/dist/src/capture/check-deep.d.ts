/**
 * Check-time fact-ness walk (carrick#707, R1d).
 *
 * The probe gates prove the two sides are not WHOLLY `any`/`unknown`/`never`.
 * They cannot see a member three levels down, and a member-level `any` accepts
 * every counterparty shape — so "no diagnostic" there is not a compatibility
 * result, it is the absence of one. The capture-time walk catches most of it,
 * but only what the capture could resolve: a member that decayed through a
 * pinned external is TypeScript's `error` placeholder at capture time and is
 * deliberately excluded there, because it heals once the check installs the
 * pin (carrick#450). This walk runs after that install, over the assembled
 * probe workspace, so it sees what the capture could not.
 *
 * It is used for exactly one thing: setting `CheckVerdict.resolved`. No bucket
 * changes because of it, so a scan's verdicts are identical with and without
 * it — what changes is whether a reader is told the verdict is a fact.
 *
 * One more reading rides the same walk (carrick#1538): the sent side's
 * unwidened alias, where a probe carries it. A member-level `any` there would
 * let that reading assign to anything, so the classifier lets it clear a
 * mismatch only when this walk ran over it and found nothing.
 */
import ts from 'typescript';
import type { TypeProvenance } from './api.js';
import { type ProbePlan } from './check-probe.js';
/** Deep findings for one pair, per probe side. */
export interface PairDeepFindings {
    sent: TypeProvenance[];
    expected: TypeProvenance[];
    /**
     * Findings on the sent side's unwidened reading (carrick#1538). Absent when
     * the probe carries none, or when its export could not be resolved.
     */
    sentUnwidened?: TypeProvenance[];
}
/** The one compiler program over the assembled probes, shared by every
 * post-judge walk (fact-ness, and the field-level report the mismatch text
 * names). Built once: two `createProgram` calls over the same file set would
 * double the check phase's most expensive step and could not disagree usefully
 * anyway. `undefined` when it cannot be built at all. */
export interface ProbeProgram {
    program: ts.Program;
    checker: ts.TypeChecker;
    probesDir: string;
}
export declare function openProbeProgram(probesDir: string, plans: ProbePlan[]): ProbeProgram | undefined;
/**
 * Walk both sides of every probe in the assembled workspace.
 *
 * Returns an empty map when the program could not be built — absence of
 * findings must never be read as "clean", so the caller treats a missing entry
 * as unresolved rather than resolved.
 */
export declare function probeDeepFindings(opened: ProbeProgram | undefined, plans: ProbePlan[]): Map<string, PairDeepFindings>;
