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
 * than this many is already better described by "this type is not typed". */
const MAX_DEEP_FINDINGS = 32;

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
 *    'error'`) is excluded (see `flagOf`): it heals when the check installs the
 *    pinned external, so it is a healable decay, not an author-baked `any`.
 * A type the walk cannot cheaply finish is NOT flagged — over-demoting a
 * legitimately fully-resolved type is the failure mode this guard must not have.
 */
export function findDisqualifyingTopTypes(
  root: ts.Type,
  program: ts.Program,
  checker: ts.TypeChecker,
  location: ts.Node
): DeepTopType[] {
  // Cover v1's inline-expander reach with margin so this structural walk is a
  // genuine superset of v1's text-scan disqualifier AT DEPTH: anything v1 could
  // expand-and-flag as `any`/`unknown`, this walk reaches too. v1's expander
  // (`MAX_EXPANSION_DEPTH` in ../type-structural-expander.ts) reaches 12; 16
  // clears it with margin. That constant is NOT imported: the capture bundle
  // seam (`capture-v2-seam.test.ts`) forbids capture/ from importing across the
  // boundary, so the value is pinned here and kept ">= MAX_EXPANSION_DEPTH" by
  // that contract. The node budget scales with the deeper bound so a real
  // deep-but-narrow type cannot exhaust it before the walk reaches a buried
  // `any` — running out early would fail OPEN (read compatible).
  const MAX_DEPTH = 16;
  const MAX_VISITED = 4096;
  const seen = new Set<ts.Type>();
  let visited = 0;

  const flagOf = (t: ts.Type): 'any' | 'unknown' | undefined => {
    if (t.flags & ts.TypeFlags.Any) {
      // TypeScript's unresolved-reference placeholder (e.g. `import('ext').Foo`
      // on a bare checkout) carries `TypeFlags.Any` but `intrinsicName ===
      // 'error'` — NOT an author-baked `any`. It resolves to the real type once
      // the check phase installs the pinned external, so it must not count as a
      // disqualifier: treating it as `any` would demote a healable external
      // reference. Genuine author `any` carries `intrinsicName === 'any'`.
      // (`intrinsicName` is internal but stable since TS 1.x — same standing as
      // its use in anchors.ts.)
      //
      // The `error` placeholder also stands in for NON-healable causes (TS2304
      // undefined name, TS2315 wrong-arity generic, a dangling internal
      // specifier). Excluding those here is not a hole: each emits a diagnostic
      // in the alias's own closure, so the closure-failure classification
      // (`internalFailure` -> decayed_internal) or the check-phase POISON rule
      // — NOT this deep walk — is their backstop, and both fail closed.
      const name = (t as unknown as { intrinsicName?: string }).intrinsicName;
      return name === 'error' ? undefined : 'any';
    }
    return t.flags & ts.TypeFlags.Unknown ? 'unknown' : undefined;
  };

  // Findings accumulate rather than short-circuiting: the FIRST is what the
  // check phase pre-gates on (so the verdict is identical to the
  // stop-at-first walk this replaces), and the rest answer "which fields are
  // `any`, and why" for a reader of the published type (carrick#376). The walk
  // stops early only on budget exhaustion, which is a fail-closed sentinel
  // about the whole type and makes any further finding meaningless.
  const found: DeepTopType[] = [];
  let exhausted = false;

  const walk = (t: ts.Type, path: string, depth: number): void => {
    // Genuine cycle handling — NOT fail-open. `t` is already on the walk stack
    // (or was fully explored earlier), so the owning frame completes it;
    // returning "clean" here is sound because a type reaches `seen` ONLY after a
    // visit that fully completed clean. A visit that hit the budget below
    // returns the `budget_exhausted` sentinel, which bubbles up (every frame
    // propagates a truthy child return) and terminates the whole walk before
    // any shallower re-entry — so `seen` never memoizes a truncated visit as
    // clean. And a type fully explored clean at depth d1 is clean at any d2 < d1
    // (the shallower reach is a superset of the deeper), so reusing it is safe.
    if (exhausted || seen.has(t)) return;
    // Budget exhaustion FAILS CLOSED. Returning "clean" here would let an
    // `any` buried past the depth/node budget read compatible — the fail-open
    // cliff a bigger number only relocates. Instead abstain: the alias demotes
    // to unverifiable, over-abstaining on a legitimately clean type
    // deeper/wider than budget rather than ever false-compatible. The sentinel
    // is placed FIRST so the check phase pre-gates on it whatever else the
    // walk had already collected.
    if (depth > MAX_DEPTH || visited > MAX_VISITED) {
      exhausted = true;
      found.unshift({ kind: 'budget_exhausted', path: path === '' ? '<root>' : path });
      return;
    }
    seen.add(t);
    visited++;

    // Root-level top types are the caller's whole-type check (and the check
    // phase's probe gates catch them); this walk owns depth > 0.
    if (depth > 0) {
      const kind = flagOf(t);
      if (kind) {
        if (found.length < MAX_DEEP_FINDINGS) found.push({ kind, path });
        return;
      }
    }

    if (t.flags & (ts.TypeFlags.Union | ts.TypeFlags.Intersection)) {
      for (const part of (t as ts.UnionOrIntersectionType).types) {
        walk(part, path, depth + 1);
        if (exhausted) return;
      }
      return;
    }

    if (!(t.flags & ts.TypeFlags.Object)) return;

    // Callable members: descend the RETURN type of every call/construct
    // signature — a return is a COVARIANT wire position, so `() => any`
    // covariantly widens `() => string` and an `any` there masks a real
    // mismatch exactly as a plain-member `any` does (v1's text scan flags it).
    // PARAMETER types are deliberately NOT descended: a parameter `any` is
    // contravariant and genuinely permissive (`(x: any) => void` safely
    // accepts a stricter counterparty), so demoting it would over-demote a
    // sound shape. Fall through afterwards so a hybrid callable
    // (`{ (): T; data: any }`) still has its own members walked below.
    for (const sig of [...t.getCallSignatures(), ...t.getConstructSignatures()]) {
      walk(sig.getReturnType(), `${path}()`, depth + 1);
      if (exhausted) return;
    }

    // Type arguments: arrays, tuples, Promise<T>, Map<K, V>, ...
    if ((t as ts.ObjectType).objectFlags & ts.ObjectFlags.Reference) {
      const args = checker.getTypeArguments(t as ts.TypeReference);
      for (let i = 0; i < args.length; i++) {
        walk(args[i], `${path}<${i}>`, depth + 1);
        if (exhausted) return;
      }
    }

    // Index signatures: { [k: string]: T }, Record<string, T>.
    for (const info of checker.getIndexInfosOfType(t)) {
      walk(info.type, `${path}[index]`, depth + 1);
      if (exhausted) return;
    }

    for (const prop of t.getProperties()) {
      // Skip the built-in method suite (`Array#map`, `Promise#then`, ...): a
      // lib-declared member is machinery, never a wire payload, and descending
      // its return type recurses (map -> U[] -> map -> ...) until it exhausts
      // the budget — which pre-fail-closed silently read "clean" and now would
      // over-abstain on every ordinary `T[]`. The element/value type is still
      // covered via the type-argument branch above; a USER function-typed
      // member (`getData: () => any`) is not lib-declared, so it is still walked.
      const decl = prop.valueDeclaration ?? prop.declarations?.[0];
      if (decl && program.isSourceFileDefaultLibrary(decl.getSourceFile())) {
        continue;
      }
      const propType = checker.getTypeOfSymbolAtLocation(prop, location);
      walk(
        propType,
        path === '' ? prop.getName() : `${path}.${prop.getName()}`,
        depth + 1
      );
      if (exhausted) return;
    }
  };

  walk(root, '', 0);
  // Property order is the checker's, which is declaration order and therefore
  // stable per input — but a stated sort is what `scan-twice.sh` byte-identity
  // rests on, so state it. The budget sentinel keeps its head position: it is
  // about the whole type, not about a member path.
  const [head, ...rest] = found;
  if (head?.kind === 'budget_exhausted') {
    rest.sort((a, b) => a.path.localeCompare(b.path));
    return [head, ...rest];
  }
  return [...found].sort((a, b) => a.path.localeCompare(b.path));
}

/**
 * Turn a deep finding into the published provenance entry (carrick#376).
 *
 * The self-check reads EMITTED declaration text, so a top type it finds is
 * text: whatever produced it — an author annotation, or an emitter that
 * printed a value it could not resolve — no install re-resolves it. That is
 * `declared`, and saying so is more useful than the bare `any` a reader gets
 * today. The one other cause it can distinguish is its own budget.
 */
export function provenanceOf(finding: DeepTopType): TypeProvenance {
  if (finding.kind === 'budget_exhausted') {
    return {
      path: finding.path,
      kind: 'budget_exhausted',
      reason: 'budget_exhausted',
      detail:
        'the type is too deep or wide to verify within the capture budget here, so it is reported unverified rather than assumed clean',
    };
  }
  return {
    path: finding.path,
    kind: finding.kind,
    reason: 'declared',
    detail: `the captured declaration states '${finding.kind}' at this position, so no counterparty shape can disagree with it`,
  };
}
