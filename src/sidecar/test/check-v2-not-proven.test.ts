/**
 * Incompatible-rule T1: the relaxed re-check's evidence.
 *
 * For a pair the judge called incompatible, a second compile re-assigns the
 * exact types the judge compared with kinds of difference that prove no break
 * allowed, one subset of kinds at a time. Its only output is `break_evidence`.
 *
 * Each seed is one cause, in invented code: the not-proven ones must fit once
 * their kind is allowed, the proven ones (a field the receiver requires and
 * the sender never sends, another kind, the controls) must fit under no
 * subset. Every seed first asserts the judge rejects its decisive line, so no
 * case passes by accident.
 *
 * The probes are judged by the compiler itself, in process: the judge's probe
 * in one program, the relaxed probe in another, their diagnostics through the
 * classifier and the evidence reader. No pnpm, no network.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import ts from 'typescript';
import { buildProbe, decisiveAssignmentLine, type ProbePlan } from '../src/capture/check-probe.js';
import { openProbeProgram } from '../src/capture/check-deep.js';
import { pairFieldReports } from '../src/capture/check-fields.js';
import { classifyPair, type RawDiagnostic } from '../src/capture/check-classify.js';
import {
  buildRelaxedProbe,
  evidenceFromDiagnostics,
  REMAINS,
  type RelaxedPlan,
} from '../src/capture/check-relaxed.js';
import type {
  BreakEvidence,
  CheckPairSpec,
  NotProvenKind,
  ProbeProtocol,
  ProbeTypeKind,
} from '../src/capture/api.js';

const TSCONFIG = JSON.stringify({
  compilerOptions: {
    strict: true,
    skipLibCheck: false,
    noEmit: true,
    module: 'esnext',
    moduleResolution: 'bundler',
    target: 'es2022',
    lib: ['es2022', 'dom'],
    baseUrl: '.',
    paths: {
      '@carrick/producer': ['./surfaces/producer.d.ts'],
      '@carrick/consumer': ['./surfaces/consumer.d.ts'],
    },
  },
  include: ['probes', 'surfaces'],
});

interface Seed {
  key: string;
  protocol: ProbeProtocol;
  type_kind: ProbeTypeKind;
  /** The producer's type (the handler's response, or what a handler or subscriber takes). */
  producer: string;
  /** The consumer's type (what a call reads, or sends). */
  consumer: string;
  /** The producer's unwidened reading (carrick#1538), when its stub ships one. */
  unwidened?: string;
}

const http = (key: string, producer: string, consumer: string): Seed => ({
  key,
  protocol: 'http',
  type_kind: 'response',
  producer,
  consumer,
});
const request = (key: string, producer: string, consumer: string): Seed => ({
  key,
  protocol: 'http',
  type_kind: 'request',
  producer,
  consumer,
});

// Two spellings of the generic JSON value a column is typed with.
const PREAMBLE = `type Doc = string | number | boolean | null | Doc[] | { [key: string]: Doc };
interface CellList extends Array<Cell> {}
type CellMap = { [K in string]?: Cell };
type Cell = string | number | boolean | CellMap | CellList | null;
`;

/** Not proven: one per false cause in the plan's evidence. */
const NOT_PROVEN: Array<{ seed: Seed; passing: NotProvenKind[][] }> = [
  {
    seed: http('openkind', `{ kind: string; id: string }`, `{ kind: 'card' | 'wire'; id: string }`),
    passing: [['open_value']],
  },
  {
    seed: http('openunderarray', `{ items: { role: string }[] }`, `{ items: { role: 'owner' | 'guest' }[] }`),
    passing: [['open_value']],
  },
  {
    seed: http(
      'openunderrecord',
      `{ byId: Record<string, { kind: string }> }`,
      `{ byId: Record<string, { kind: 'card' | 'wire' }> }`
    ),
    passing: [['open_value']],
  },
  {
    seed: http('jsonobject', `{ meta: Doc; id: string }`, `{ meta: { total: number }; id: string }`),
    passing: [['open_value']],
  },
  {
    seed: http('jsonarray', `{ meta: Doc }`, `{ meta: { label: string }[] }`),
    passing: [['open_value']],
  },
  {
    // The mapped spelling, beside a Date the wire form turns into a string.
    seed: http('jsonmapped', `{ meta: Cell; createdOn: Date }`, `{ meta: { label: string }; createdOn: string }`),
    passing: [['open_value']],
  },
  {
    seed: http('nulloptional', `{ workspaceRef: string | null }`, `{ workspaceRef?: string }`),
    passing: [['null_where_optional']],
  },
  {
    seed: http('nullrequired', `{ createdOn: string | null }[]`, `{ createdOn: string }[]`),
    passing: [['null_where_required']],
  },
  {
    seed: http('weakcast', `{ order: { id: string; total: number } }`, `{ error?: string }`),
    passing: [['weak_type']],
  },
  {
    // A null where a value is required, of a type whose every field is optional.
    seed: http('nullweak', `{ opts: { a: string } | null }`, `{ opts: { b?: string } }`),
    passing: [['null_where_required', 'weak_type']],
  },
  {
    // Two causes on one pair: both must be allowed.
    seed: http(
      'openandnull',
      `{ kind: string; ref: string | null }`,
      `{ kind: 'card' | 'wire'; ref?: string }`
    ),
    passing: [['open_value', 'null_where_optional']],
  },
  {
    seed: request('requestopen', `{ variant: 'solo' | 'group' }`, `{ variant: string }`),
    passing: [['open_value']],
  },
  {
    seed: request('requestnull', `{ logo?: string; name: string }`, `{ logo: string | null; name: string }`),
    passing: [['null_where_optional']],
  },
  {
    seed: { key: 'socketnull', protocol: 'socket', type_kind: 'both', producer: `{ room?: string }`, consumer: `{ room: string | null }` },
    passing: [['null_where_optional']],
  },
  {
    seed: { key: 'pubsubopen', protocol: 'pubsub', type_kind: 'both', producer: `{ tier: 'gold' | 'silver' }`, consumer: `{ tier: string }` },
    passing: [['open_value']],
  },
  {
    seed: { key: 'graphqlleaf', protocol: 'graphql', type_kind: 'response', producer: `{ status: string; id: string }`, consumer: `{ status: 'ACTIVE' | 'PAUSED'; id: string }` },
    passing: [['open_value']],
  },
  {
    // The producer's own type lists a value the consumer's does not: proven
    // under every V1 kind, and fits only once `listed_value` is allowed.
    seed: http('listedvalue', `{ state: 'open' | 'closed' | 'held' }`, `{ state: 'open' | 'closed' }`),
    passing: [['listed_value']],
  },
  {
    // The published type is open, and the handler's unwidened reading lists
    // a value the consumer lacks: both forms must fit, so the open value
    // alone does not clear it.
    seed: {
      ...http('unwidenedlisted', `{ state: string }`, `{ state: 'open' | 'closed' }`),
      unwidened: `{ state: 'open' | 'held' }`,
    },
    passing: [['open_value', 'listed_value']],
  },
];

/** Proven: the true breaks and the controls. No subset of kinds clears them. */
const PROVEN: Seed[] = [
  http('missingfield', `{ id: string }`, `{ id: string; openedAt: string }`),
  http('missingbesideopen', `{ id: string; kind: string }`, `{ id: string; kind: 'card' | 'wire'; openedAt: string }`),
  http('wrongkind', `{ count: string }`, `{ count: number }`),
  http('arrayforobject', `{ tags: string[] }`, `{ tags: { name: string } }`),
  http('missingunderarray', `{ rows: { id: string }[] }`, `{ rows: { id: string; openedAt: string }[] }`),
  http('missingundernullable', `{ owner: { id: string } | null }`, `{ owner: { id: string; name: string } | null }`),
  http('optionalonsender', `{ openedAt?: string }`, `{ openedAt: string }`),
  http('undefinedonsender', `{ openedAt: string | undefined }`, `{ openedAt: string }`),
  http('errorjoined', `{ error: string } | { id: string; kind: string }`, `{ id: string; kind: 'card' | 'wire' }`),
  http('nomemberfits', `{ error: string } | { ok: true; id: number }`, `{ ok: true; id: string }`),
  http('stringforobject', `{ meta: string }`, `{ meta: { total: number } }`),
  http('jsonforstring', `{ meta: Doc }`, `{ meta: string }`),
  http('tupleorder', `{ pair: [string, number] }`, `{ pair: [number, string] }`),
  http('tuplelength', `{ pair: [string] }`, `{ pair: [string, string] }`),
  request('requestmissing', `{ variant: 'solo' | 'group'; seats: number }`, `{ variant: string }`),
  { key: 'socketwrongkind', protocol: 'socket', type_kind: 'both', producer: `{ room: number }`, consumer: `{ room: string | null }` },
  { key: 'graphqlmissing', protocol: 'graphql', type_kind: 'response', producer: `{ id: string }`, consumer: `{ id: string; status: 'ACTIVE' }` },
];

const SEEDS = [...NOT_PROVEN.map((n) => n.seed), ...PROVEN];

/** The four kinds the plan's V1 table treats as not proven (D7: R6, R11, R13 and R4 stay). */
const V1: NotProvenKind[] = ['open_value', 'null_where_optional', 'null_where_required', 'weak_type'];

/** A relabel's read of the evidence that allows `allowed`: demoted only when a passing set lies inside it. */
function demotes(e: BreakEvidence, allowed: NotProvenKind[]): boolean {
  return e.passing.some((set) => set.every((k) => allowed.includes(k)));
}

function spec(seed: Seed): CheckPairSpec {
  return {
    pair_key: seed.key,
    protocol: seed.protocol,
    type_kind: seed.type_kind,
    producer: { service_name: 'producer', alias: `${seed.key}_Producer` },
    consumer: { service_name: 'consumer', alias: `${seed.key}_Consumer` },
  };
}

function diagnosticsOf(dir: string, files: string[], sub: string): Map<string, RawDiagnostic[]> {
  const raw = ts.readConfigFile(path.join(dir, 'tsconfig.json'), (f) => fs.readFileSync(f, 'utf8'));
  const parsed = ts.parseJsonConfigFileContent(raw.config, ts.sys, dir);
  const program = ts.createProgram(
    files.map((f) => path.join(dir, sub, f)),
    parsed.options
  );
  const out = new Map<string, RawDiagnostic[]>();
  for (const f of files) {
    const source = program.getSourceFile(path.join(dir, sub, f));
    assert.ok(source, `${f} must be in the program`);
    out.set(
      f,
      program.getSemanticDiagnostics(source).map((d) => ({
        file: `${sub}/${f}`,
        line: source.getLineAndCharacterOfPosition(d.start ?? 0).line + 1,
        col: source.getLineAndCharacterOfPosition(d.start ?? 0).character + 1,
        code: d.code,
        message: ts.flattenDiagnosticMessageText(d.messageText, '\n'),
      }))
    );
  }
  return out;
}

describe('the relaxed re-check: evidence for incompatible pairs (incompatible-rule T1)', () => {
  let dir: string;
  const plans = new Map<string, ProbePlan>();
  const firstPass = new Map<string, RawDiagnostic[]>();
  const relaxed = new Map<string, RelaxedPlan | { cause: string }>();
  const evidence = new Map<string, BreakEvidence>();

  before(() => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-not-proven-'));
    for (const sub of ['probes', 'surfaces', 'relaxed']) fs.mkdirSync(path.join(dir, sub));
    fs.writeFileSync(path.join(dir, 'tsconfig.json'), TSCONFIG);
    fs.writeFileSync(
      path.join(dir, 'surfaces', 'producer.d.ts'),
      PREAMBLE +
        SEEDS.map((s) => `export type ${s.key}_Producer = ${s.producer};`).join('\n') +
        '\n' +
        SEEDS.filter((s) => s.unwidened)
          .map((s) => `export type ${s.key}_Producer_Unwidened = ${s.unwidened};`)
          .join('\n') +
        '\n'
    );
    fs.writeFileSync(
      path.join(dir, 'surfaces', 'consumer.d.ts'),
      PREAMBLE + SEEDS.map((s) => `export type ${s.key}_Consumer = ${s.consumer};`).join('\n') + '\n'
    );
    for (const seed of SEEDS) {
      const plan = buildProbe(spec(seed), (s) => `@carrick/${s}`, (sent) =>
        seed.unwidened ? `${sent.alias}_Unwidened` : undefined
      );
      plans.set(seed.key, plan);
      fs.writeFileSync(path.join(dir, 'probes', plan.fileName), plan.source);
    }
    const judged = diagnosticsOf(dir, [...plans.values()].map((p) => p.fileName), 'probes');
    for (const [key, plan] of plans) firstPass.set(key, judged.get(plan.fileName)!);
    for (const [key, plan] of plans) {
      const built = buildRelaxedProbe(plan, firstPass.get(key)!);
      relaxed.set(key, built);
      if (!('cause' in built)) fs.writeFileSync(path.join(dir, 'relaxed', built.fileName), built.source);
    }
    const built = [...relaxed.values()].filter((r): r is RelaxedPlan => !('cause' in r));
    const second = diagnosticsOf(dir, built.map((r) => r.fileName), 'relaxed');
    for (const r of built) evidence.set(r.pairId, evidenceFromDiagnostics(r, second.get(r.fileName)!));
  });

  after(() => fs.rmSync(dir, { recursive: true, force: true }));

  /** The judge's verdict, asserted incompatible on the decisive line. */
  function judged(key: string) {
    const plan = plans.get(key)!;
    const probeDiags = firstPass.get(key)!;
    assert.ok(
      probeDiags.some((d) => d.line === decisiveAssignmentLine(plan)),
      `${key}: the judge must reject the decisive assignment: ${JSON.stringify(probeDiags)}`
    );
    const opened = openProbeProgram(dir, [plan]);
    const verdict = classifyPair({
      plan,
      probeDiags,
      poisonReason: () => undefined,
      scrubCtx: { workspaceRoot: dir, packageLabelOf: () => undefined },
      deepFindings: { sent: [], expected: [] },
      fieldReport: pairFieldReports(opened, [plan]).get(plan.pairId),
    });
    assert.strictEqual(verdict.bucket, 'incompatible', `${key}: ${JSON.stringify(verdict)}`);
    const e = evidence.get(plan.pairId);
    assert.ok(e, `${key}: no evidence: ${JSON.stringify(relaxed.get(key))}`);
    return e;
  }

  describe('not proven: the pair fits once its kind is allowed', () => {
    for (const { seed, passing } of NOT_PROVEN) {
      it(`${seed.protocol} ${seed.type_kind}: ${seed.key}`, () => {
        const e = judged(seed.key);
        // A pair that fits only with a value the producer's own type lists
        // stays proven: `proven` reads the plan's kinds only.
        const listed = passing.every((set) => set.includes('listed_value'));
        assert.deepStrictEqual(
          { proven: e.proven, passing: e.passing, kinds: e.kinds },
          { proven: listed, passing, kinds: listed ? [] : passing[0] }
        );
        if (listed) assert.match(e.cause!, /listed_value allowed, outside the kinds that prove no break/);
        assert.strictEqual(demotes(e, V1), !listed, `${seed.key} under the V1 table`);
        assert.strictEqual(demotes(e, [...V1, 'listed_value']), true, `${seed.key} with every kind allowed`);
      });
    }
  });

  describe('proven: no subset of kinds clears the pair', () => {
    for (const seed of PROVEN) {
      it(`${seed.protocol} ${seed.type_kind}: ${seed.key}`, () => {
        const e = judged(seed.key);
        assert.deepStrictEqual(e, {
          proven: true,
          kinds: [],
          passing: [],
          cause: REMAINS,
        });
      });
    }
  });

  describe('each kind is its own switch', () => {
    it('a pair needing two kinds is demoted only when both are allowed', () => {
      const e = judged('openandnull');
      assert.strictEqual(demotes(e, ['open_value']), false);
      assert.strictEqual(demotes(e, ['null_where_optional']), false);
      assert.strictEqual(demotes(e, ['open_value', 'null_where_optional']), true);
    });

    it('turning off the null-where-required kind keeps a required null proven', () => {
      const e = judged('nullrequired');
      assert.strictEqual(demotes(e, V1.filter((k) => k !== 'null_where_required')), false);
      assert.strictEqual(demotes(judged('nulloptional'), V1.filter((k) => k !== 'null_where_required')), true);
    });
  });
});

describe('the evidence reader fails closed (incompatible-rule T1)', () => {
  // A relaxed plan with one comparison: canary on line 3, block from line 10,
  // subset masks 0..31 on lines 20..51.
  function plan(): RelaxedPlan {
    const subsetLines = new Map<number, { comparison: number; mask: number }>();
    for (let mask = 0; mask < 32; mask++) subsetLines.set(20 + mask, { comparison: 0, mask });
    return {
      pairId: 'p',
      fileName: 'pair_p.ts',
      source: '',
      fromLine: 10,
      canaryLines: [3],
      subsetLines,
      comparisons: 1,
      firstPass: ['3:2322'],
    };
  }
  const diag = (line: number, code = 2322): RawDiagnostic => ({ file: 'x', line, col: 1, code, message: '' });
  const canary = diag(3);
  /** Every subset fails except those containing `open_value` (mask bit 0). */
  const openOnly = () => {
    const out = [canary];
    for (let mask = 0; mask < 32; mask++) if ((mask & 1) === 0) out.push(diag(20 + mask));
    return out;
  };

  it('reads open_value from a clean run', () => {
    assert.deepStrictEqual(evidenceFromDiagnostics(plan(), openOnly()), {
      proven: false,
      kinds: ['open_value'],
      passing: [['open_value']],
    });
  });

  it("the judge's own mismatch missing from the relaxed compile: proven", () => {
    const e = evidenceFromDiagnostics(plan(), openOnly().slice(1));
    assert.strictEqual(e.proven, true);
    assert.match(e.cause!, /did not raise the judge's own diagnostics/);
  });

  it('a too-deep instantiation on a helper line: proven', () => {
    const e = evidenceFromDiagnostics(plan(), [...openOnly(), diag(12, 2589)]);
    assert.strictEqual(e.proven, true);
    assert.match(e.cause!, /TS2589 outside its assignments/);
  });

  it('a too-deep instantiation on the all-kinds line: proven', () => {
    const e = evidenceFromDiagnostics(plan(), [...openOnly(), diag(20 + 31, 2589)]);
    assert.strictEqual(e.proven, true);
  });

  it('the empty subset passing (the helpers changed the comparison): proven', () => {
    const e = evidenceFromDiagnostics(plan(), openOnly().filter((d) => d.line !== 20));
    assert.strictEqual(e.proven, true);
    assert.match(e.cause!, /with no relaxation allowed/);
  });

  it('a superset failing where its subset passed: proven', () => {
    const e = evidenceFromDiagnostics(plan(), [...openOnly(), diag(20 + 3)]);
    assert.strictEqual(e.proven, true);
    assert.match(e.cause!, /relaxations disagreed/);
  });

  it('a new diagnostic above the block that the judge did not raise: proven', () => {
    const e = evidenceFromDiagnostics(plan(), [...openOnly(), diag(5, 2304)]);
    assert.strictEqual(e.proven, true);
  });
});
