/**
 * Diagnostic parsing + four-bucket classification for the v2 check phase.
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
import type { GateName, ProbePlan, Side } from './check-probe.js';
import { scrubDiagnostic, type ScrubContext } from './check-scrub.js';
import type { PairDeepFindings } from './check-deep.js';

export interface RawDiagnostic {
  /** Workspace-relative, forward-slash file path (empty for global errors). */
  file: string;
  line: number;
  col: number;
  code: number;
  /** Primary text plus any indented elaboration lines, joined with '\n'. */
  message: string;
}

const PRIMARY_RE =
  /^(?<file>(?:[a-zA-Z]:)?[^(]*?)\((?<line>\d+),(?<col>\d+)\): error TS(?<code>\d+): (?<msg>.*)$/;

/** Parse `tsc --pretty false` output into structured diagnostics. */
export function parseTscOutput(stdout: string): RawDiagnostic[] {
  const diags: RawDiagnostic[] = [];
  let current: RawDiagnostic | null = null;
  for (const rawLine of stdout.split('\n')) {
    const line = rawLine.replace(/\r$/, '');
    const m = line.match(PRIMARY_RE);
    if (m && m.groups) {
      current = {
        file: m.groups.file.split('\\').join('/'),
        line: Number(m.groups.line),
        col: Number(m.groups.col),
        code: Number(m.groups.code),
        message: m.groups.msg,
      };
      diags.push(current);
      continue;
    }
    // Indented continuation lines belong to the preceding primary diagnostic.
    if (current && /^\s+\S/.test(line)) {
      current.message += '\n' + line;
      continue;
    }
    // Blank line or summary ("Found N errors.") ends the current run.
    current = null;
  }
  return diags;
}

/** Assignment-class codes: a real structural mismatch on the value assignment. */
const ASSIGNMENT_CODES = new Set([2322, 2559, 2739, 2740, 2741, 2769, 2345]);

function sideForGate(name: GateName, plan: ProbePlan): { side: Side; kind: string } {
  const [which, kind] = name.split(':');
  const side = which === 'sent' ? plan.direction.sent : plan.direction.expected;
  return { side, kind };
}

function endpointAliasFor(side: Side, plan: ProbePlan): string {
  return side === plan.direction.sent
    ? plan.sentEndpoint.alias
    : plan.expectedEndpoint.alias;
}

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
}

/** Classify one pair into exactly one bucket, honouring the precedence order. */
export function classifyPair(input: ClassifyInput): CheckVerdict {
  const { plan, probeDiags, poisonReason, scrubCtx } = input;
  const codes = [...new Set(probeDiags.map((d) => d.code))].sort((a, b) => a - b);
  const base = { pair_id: plan.pairId, pair_key: plan.spec.pair_key, codes };
  // Every branch below this point except the last two returns a verdict about
  // a type nobody could read; each states that in one place rather than
  // repeating the reasoning.
  const notAFact = (reason: string) => ({ resolved: false as const, unresolved_reason: reason });

  // 1. Poison: a diagnostic in this pair's own alias closure (either side)
  //    makes the pair unverifiable, never "no probe error -> compatible". A
  //    sibling alias's poison in the same service no longer reaches here.
  for (const side of ['producer', 'consumer'] as const) {
    const endpoint = plan.spec[side];
    const reason = poisonReason(endpoint.service_name, endpoint.alias);
    if (reason) {
      return {
        ...base,
        bucket: 'unverifiable',
        gate: `poison:${side}`,
        diagnostic: `the type stub for service '${endpoint.service_name}' does not typecheck (its own declarations carry diagnostics); compatibility cannot be verified.`,
        ...notAFact(`the ${side} stub does not typecheck`),
      };
    }
  }

  // 2. Surface import error (missing/renamed export): probe import lines.
  const importDiag = probeDiags.find((d) => plan.importLines.includes(d.line));
  if (importDiag) {
    const side: Side = plan.importLines[0] === importDiag.line ? plan.direction.sent : plan.direction.expected;
    const alias = endpointAliasFor(side, plan);
    return {
      ...base,
      bucket: 'unverifiable',
      gate: `import:${side}`,
      diagnostic: `surface export '${alias}' for the ${side} is missing or renamed; compatibility cannot be verified.`,
      ...notAFact(`the ${side} surface export is missing or renamed`),
    };
  }

  // 3/4. Probe gates (TS2344). IsAny outranks IsUnknown/IsNever.
  const gateDiags = probeDiags.filter(
    (d) => d.code === 2344 && plan.gateLines.has(d.line)
  );
  const anyGate = gateDiags
    .map((d) => plan.gateLines.get(d.line)!)
    .find((name) => name.endsWith(':any'));
  if (anyGate) {
    const { side } = sideForGate(anyGate, plan);
    return {
      ...base,
      bucket: 'gate_caught_baked_any',
      gate: `${side}:any`,
      diagnostic: `the ${side} type resolved to 'any' at check time; compatibility cannot be verified (a type inferred through a missing library bakes to any).`,
      ...notAFact(`the ${side} type is 'any'`),
    };
  }
  const decayGate = gateDiags
    .map((d) => plan.gateLines.get(d.line)!)
    .find((name) => name.endsWith(':unknown') || name.endsWith(':never'));
  if (decayGate) {
    const { side, kind } = sideForGate(decayGate, plan);
    return {
      ...base,
      bucket: 'unverifiable',
      gate: `${side}:${kind}`,
      diagnostic: `the ${side} type resolved to '${kind}' at check time; compatibility cannot be verified.`,
      ...notAFact(`the ${side} type is '${kind}'`),
    };
  }

  // 5. Assignment-class error on the value assignment line -> incompatible.
  const assignDiag = probeDiags.find(
    (d) => d.line === plan.assignmentLine && ASSIGNMENT_CODES.has(d.code)
  );
  if (assignDiag) {
    return {
      ...base,
      bucket: 'incompatible',
      diagnostic: scrubDiagnostic(
        assignDiag.message,
        scrubCtx,
        plan.sentEndpoint.alias,
        plan.expectedEndpoint.alias
      ),
      ...factness(input),
    };
  }

  // Any other diagnostic on the assignment line that is not a known assignment
  // code still means the pair could not be cleanly verified.
  const otherAssign = probeDiags.find((d) => d.line === plan.assignmentLine);
  if (otherAssign) {
    return {
      ...base,
      bucket: 'unverifiable',
      gate: 'assignment:other',
      diagnostic: scrubDiagnostic(
        otherAssign.message,
        scrubCtx,
        plan.sentEndpoint.alias,
        plan.expectedEndpoint.alias
      ),
      ...notAFact('the probe raised a diagnostic that is not an assignment mismatch'),
    };
  }

  // 6. No diagnostics -> compatible.
  return { ...base, bucket: 'compatible', ...factness(input) };
}

/**
 * Whether a compared pair is a FACT about two known types (carrick#707, R1d).
 *
 * The probe gates only rule out a WHOLLY top-typed side. A pair whose producer
 * declares `{ id: string; meta: any }` clears every gate and then reads
 * compatible against literally any counterparty `meta`, because `any` is
 * bidirectionally assignable. That is not a compatibility result, and a reader
 * who acts on it acts on nothing. So `resolved` additionally requires the deep
 * walk to have RUN over both sides in the assembled workspace and come back
 * empty. A walk that could not run leaves the verdict not-a-fact: absence of
 * findings is not evidence.
 */
function factness(input: ClassifyInput): {
  resolved: boolean;
  unresolved_reason?: string;
} {
  const findings = input.deepFindings;
  if (!findings) {
    return {
      resolved: false,
      unresolved_reason:
        'the type could not be walked for member-level any/unknown after install, so the comparison is not established as a fact',
    };
  }
  for (const side of ['sent', 'expected'] as const) {
    const first = findings[side][0];
    if (!first) continue;
    const label = side === 'sent' ? input.plan.direction.sent : input.plan.direction.expected;
    const where = first.path === '' ? 'its root' : `'${first.path}'`;
    return {
      resolved: false,
      unresolved_reason:
        first.kind === 'budget_exhausted'
          ? `the ${label} type is too deep or wide to verify at ${where}`
          : `the ${label} type carries '${first.kind}' at ${where}, which every counterparty shape satisfies`,
    };
  }
  return { resolved: true };
}
