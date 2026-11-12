/**
 * The structural any/unknown walk, shared by the two type surfaces that need it
 * (carrick#448, carrick#707).
 *
 * Capture-time it runs over the emitted stub tree and decides whether an alias
 * carries a member-level decay the check phase must pre-gate. Check-time it
 * runs over the assembled probe workspace, where the pinned externals ARE
 * installed, and decides whether a verdict is a fact about two known types or
 * an artefact of a type nobody could see (`CheckVerdict.resolved`). The
 * whole-type probe gates cannot answer that: `any` three members down passes
 * every one of them.
 *
 * One walk, one budget, one path notation, so a finding means the same thing
 * wherever it is reported.
 */
import ts from 'typescript';
import type { TypeProvenance } from './api.js';
/** A disqualifying finding below the root of a captured type: an author-baked
 * `any`/`unknown`, or `budget_exhausted` — a subtree the walk could not finish
 * within its depth/node budget, treated as unverifiable (fail closed) rather
 * than silently clean. */
export type DeepTopType = Pick<TypeProvenance, 'kind' | 'path'>;
/** Cap on findings reported per alias. The FIRST one is what the check phase
 * pre-gates on, so verdicts never depend on this number; the rest are there to
 * tell a reader which fields are `any` (carrick#376), and a type with more
 * than this many is already better described by "this type is not typed".
 * The scanner mirrors it (`CAPTURE_FINDINGS_CAP` in
 * `src/engine/type_compat_v2.rs`): a record this long may have stopped
 * listing, so it cannot vouch for every position (carrick#1752). */
export declare const MAX_DEEP_FINDINGS = 32;
/**
 * Depth-bounded structural walk for a disqualifying top type at ANY depth:
 * a member, array element, index signature, type argument, or callable RETURN
 * that resolved to `any` (bidirectionally assignable — an arbitrary
 * counterparty shape reads compatible) or `unknown` (a failed-inference bake;
 * carries no shape information). The check phase's probe gates are WHOLE-type
 * only, so this walk owns the depths they cannot see. It is a genuine SUPERSET
 * of v1's text-scan disqualifier (`contains_disqualifying_top_type`) at ALL
 * depths/widths — so removing the `type_state == Unknown` pre-verdict (carrick
 * #448) never turns a shape v1 would have abstained on into a false-compatible.
 * The superset holds unconditionally because budget EXHAUSTION FAILS CLOSED
 * (returns the `budget_exhausted` sentinel, not "clean"): a subtree too deep or
 * wide to finish within budget is unverifiable, never silently compatible. A
 * bigger bound would only relocate the fail-open cliff; failing closed removes
 * it. v1's text scan is itself unbounded, so any disqualifier it would flag
 * that lies past this walk's finite budget is still caught — as
 * `budget_exhausted` rather than its exact kind.
 *
 * Cycle-safe. Two deliberate exceptions to "flag any/unknown anywhere":
 *  - callable PARAMETER types are NOT descended (only return types): a
 *    parameter `any` is contravariant and genuinely permissive (`(x: any) =>
 *    void` safely accepts a stricter counterparty), so it is not a masked
 *    mismatch and demoting it would over-demote a sound shape;
 *  - TypeScript's unresolved-reference `error` placeholder (`intrinsicName ===
 *    'error'`) is excluded (see `disqualifyingFlag`): it heals when the check installs the
 *    pinned external, so it is a healable decay, not an author-baked `any`.
 * A type the walk cannot cheaply finish is NOT flagged — over-demoting a
 * legitimately fully-resolved type is the failure mode this guard must not have.
 */
export declare function findDisqualifyingTopTypes(root: ts.Type, program: ts.Program, checker: ts.TypeChecker, location: ts.Node): DeepTopType[];
/**
 * Member paths at which `root` holds TypeScript's unresolved-reference
 * placeholder (the `error` intrinsic) rather than a type (carrick#1164).
 *
 * Run on the SOURCE program, where a member typed through an import that did
 * not resolve still carries the placeholder. Once printed into the stub the
 * placeholder is the keyword `any`, indistinguishable from an author's `any`,
 * so this is the only point at which the two causes can be told apart. Same
 * walk, same budget and the same path notation as the self-check's walk over
 * the emitted text, so a path found here names the member a self-check finding
 * names. A walk that runs out of budget reports what it found before it did.
 */
export declare function findUnresolvedPlaceholders(root: ts.Type, program: ts.Program, checker: ts.TypeChecker, location: ts.Node): string[];
/**
 * The member paths of `root` whose type is declared in a file `uncarried`
 * names (carrick#2418): a referenced project's source, which the anchor
 * program reads but the surface emit, under the compiler's own resolution,
 * reaches only as an unbuilt output. Where the emitted text then holds a top
 * type, its cause is the import the emit could not follow, as it would be for
 * an unresolved placeholder. The walk stops at the first such position on
 * each path.
 */
export declare function findUncarriedPositions(root: ts.Type, program: ts.Program, checker: ts.TypeChecker, location: ts.Node, uncarried: (fileName: string) => boolean): string[];
/** TypeScript's unresolved-reference placeholder: `TypeFlags.Any` with the
 * internal `intrinsicName === 'error'` (stable since TS 1.x; see `anchors.ts`). */
export declare function isErrorPlaceholder(t: ts.Type): boolean;
/**
 * True when a deep-walk path passes through a named member (the same rule as
 * `path_names_a_member` in `src/engine/type_compat_v2.rs`). The walk writes a
 * member as its name, an element or type argument as `<n>`, an index signature
 * as `[index]` and a callable return as `()`; a path made of the last three
 * alone sits at the root of the type or inside a container, never in a field.
 */
export declare function pathNamesAMember(path: string): boolean;
/**
 * True when every finding is a top type the SOURCE declares under a named
 * member: an OPEN member (carrick#2591). The policy: an open member is never
 * a mismatch, and the closed members are judged as for a fully typed pair.
 * `any` accepts and supplies every shape in the assignment the probe makes, so
 * the compiler already judges only the closed members.
 *
 * `unknown` is open only on the side that RECEIVES the payload
 * (`receiving`): any type is assignable to `unknown`, so no mismatch can hide
 * there. On the sending side `unknown` rejects a narrower receiver, so it
 * would read as a mismatch the policy forbids and stays a gate, except where
 * the `counterpart` (the receiving side's findings) declares a top type at the
 * same path: anything is assignable there.
 *
 * Nothing else qualifies. A callable's return (`getData()`) is no data
 * member. A `budget_exhausted` sentinel, a root top type and a top type an
 * unresolved import left say nothing about the author's intent. A list as long
 * as the walk's cap may have stopped listing, so it cannot vouch for every
 * position.
 */
export declare function onlyDeclaredOpenMembers(findings: readonly TypeProvenance[], receiving: boolean, counterpart?: readonly TypeProvenance[]): boolean;
/** What an anchor's SOURCE program could not resolve (carrick#1164). */
export interface UnresolvedAtAnchor {
    /** Member paths holding the unresolved-reference placeholder. */
    paths: readonly string[];
    /**
     * Module specifiers, as the source wrote them, that did not resolve from the
     * anchor's file or a module it reaches. Internal specifiers first. May be
     * empty: a name the program never declared leaves the same placeholder.
     */
    specifiers: readonly string[];
}
/**
 * Turn a deep finding into the published provenance entry (carrick#376).
 *
 * The self-check reads EMITTED declaration text, so a top type it finds is
 * text: an author annotation and an emitter that printed an unresolved value
 * both read `any` there. The anchor's source program can still tell them apart
 * (`findUnresolvedPlaceholders`), and when it recorded the finding's path as a
 * placeholder the cause is `unresolved_import`: a dependency or a generated
 * module was missing on the scanned checkout, which installing or generating
 * it fixes (carrick#1164). Anything else at that position is `declared`. The
 * one other cause the walk itself can distinguish is its own budget.
 */
export declare function provenanceOf(finding: DeepTopType, unresolved?: UnresolvedAtAnchor): TypeProvenance;
/**
 * The entry for one position at which the emitted tree holds the
 * unresolved-reference placeholder (carrick#1446): `path` is `''` for the
 * alias's own type. `names` are the identifiers the self-check could not find
 * in this alias's statement and the files it reaches.
 *
 * The cause is `unresolved_import`, the word a reader already has for "a
 * reference here did not resolve"; the detail says where it failed to.
 */
export declare function unresolvedInTreeProvenance(path: string, names: readonly string[]): TypeProvenance;
/**
 * The root entry for a literal anchor demoted because its text names a module
 * whose declaration emit was skipped (carrick#1446, carrick#1165): that text is
 * what the index serves for it, and nothing in the emitted tree declares what
 * it imports.
 */
export declare function unemittedModuleProvenance(): TypeProvenance;
