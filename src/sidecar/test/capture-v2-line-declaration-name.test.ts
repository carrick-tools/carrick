/**
 * carrick#1785: the capture's line fallback never serves a declaration's own
 * type.
 *
 * When an infer anchor's span and expression text name nothing (or it carries
 * a line alone), `locateNode` falls back to the first expression on the line.
 * `ts.isExpression` is true for every identifier, including a declaration's
 * NAME, which a pre-order walk reaches before the declared body. So on a line
 * that declares a type alias, an interface, a class or a function, the
 * fallback read the declared entity's name and the capture published that
 * entity's whole type. That is how carrick#1775 served a whole GraphQL
 * operation under a field-keyed row.
 *
 * A line that declares something names the declaration, not a payload: the
 * anchor abstains (`unknown`, the reason on `self_check_detail`, nothing for
 * the backfill to re-anchor). It does not walk on to the next node either: on
 * a function's line that is the first parameter, a second wrong answer that
 * self-checks clean.
 *
 * A binding whose name IS the value read at that position (a destructured
 * element, a shorthand property) is not a declaration in this sense and keeps
 * resolving.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord } from '../src/capture/api.js';

const MONEY_TS = `export type Currency = 'EUR' | 'USD';
export function formatTotal(total: number): string {
  return String(total);
}
export default { rate: 1 };
`;

const LEDGER_TS = `import { formatTotal, type Currency } from './money';
import rates from './money';
import * as moneyNs from './money';

export type LedgerSummary = { total: number; currency: Currency };
export interface LedgerRow { id: string; amount: number }
export interface LedgerFilter {
  kind: 'open' | 'closed';
  matches(row: LedgerRow): boolean;
}
export enum LedgerState {
  Open = 'open',
  Closed = 'closed',
}
export namespace Ledgers { export const version = 1; }

export function listLedgers(filter: LedgerFilter): LedgerRow[] {
  return filter.kind === 'open' ? [] : [];
}

export class LedgerStore {
  rows: LedgerRow[] = [];
  async load(filter: LedgerFilter): Promise<LedgerRow[]> {
    return filter.kind === 'open' ? this.rows : [];
  }
  get size(): number {
    return this.rows.length;
  }
  set size(count: number) {
    this.rows.length = count;
  }
}

export { listLedgers as list };

export function pickLedgers(
  filter: LedgerFilter,
): LedgerRow[] {
  return filter.kind === 'open' ? [] : [];
}

declare function fetchLedger(): Promise<{ summary: LedgerSummary; rows: LedgerRow[] }>;

export async function loadLedger(): Promise<LedgerRow[]> {
  const {
    summary,
  } = await fetchLedger();
  formatTotal(summary.total);
  const fetched = await fetchLedger();
  return fetched.rows;
}

export function ledgerBody(totals: LedgerSummary) {
  return {
    totals,
  };
}
`;

const SOURCE_REL = 'src/ledger.ts';

/** 1-based line of the sole line of the fixture that starts with `text`. */
function lineOf(text: string): number {
  const lines = LEDGER_TS.split('\n');
  const hits = lines
    .map((l, i) => (l.startsWith(text) ? i + 1 : 0))
    .filter((n) => n > 0);
  assert.strictEqual(hits.length, 1, `fixture must hold exactly one line starting with: ${text}`);
  return hits[0];
}

/**
 * Line-only anchors on declaration lines: alias -> [line prefix, the
 * declaration kind the reason must name].
 */
const DECLARATION_LINES: Record<string, [string, string]> = {
  Endpoint_typeAlias_Response: ['export type LedgerSummary', 'TypeAliasDeclaration'],
  Endpoint_interface_Response: ['export interface LedgerRow', 'InterfaceDeclaration'],
  Endpoint_propertySignature_Response: ["  kind: 'open'", 'PropertySignature'],
  Endpoint_methodSignature_Response: ['  matches(row', 'MethodSignature'],
  Endpoint_enum_Response: ['export enum LedgerState', 'EnumDeclaration'],
  Endpoint_enumMember_Response: ["  Open = 'open',", 'EnumMember'],
  Endpoint_namespace_Response: ['export namespace Ledgers', 'ModuleDeclaration'],
  Endpoint_function_Response: ['export function listLedgers', 'FunctionDeclaration'],
  Endpoint_class_Response: ['export class LedgerStore', 'ClassDeclaration'],
  Endpoint_propertyDeclaration_Response: ['  rows: LedgerRow[] = [];', 'PropertyDeclaration'],
  Endpoint_method_Response: ['  async load(', 'MethodDeclaration'],
  Endpoint_getAccessor_Response: ['  get size()', 'GetAccessor'],
  Endpoint_setAccessor_Response: ['  set size(', 'SetAccessor'],
  Endpoint_exportSpecifier_Response: ['export { listLedgers as list }', 'ExportSpecifier'],
  Endpoint_importSpecifier_Response: ['import { formatTotal', 'ImportSpecifier'],
  Endpoint_importClause_Response: ['import rates', 'ImportClause'],
  Endpoint_namespaceImport_Response: ['import * as moneyNs', 'NamespaceImport'],
};

/** Line-only anchors on lines whose first node is a value: alias -> [line prefix, a member the shape must carry]. */
const VALUE_LINES: Record<string, [string, RegExp]> = {
  // A destructured element: its name IS the value read there.
  Endpoint_bindingElement_Response: ['    summary,', /total: number/],
  // A shorthand property: its name is also the expression it reads.
  Endpoint_shorthand_Response: ['    totals,', /total: number/],
  // The consumer call-result shape (carrick#788): the variable, not its name.
  Endpoint_variable_Response: ['  const fetched = await fetchLedger();', /rows: /],
  // A parameter on its own line is a child of a function, not its name.
  Endpoint_parameter_Response: ['  filter: LedgerFilter,', /LedgerFilter/],
};

const PARTIAL_SPAN_ALIAS = 'Endpoint_partialSpan_Response';
const NAMED_BY_SPAN_ALIAS = 'Endpoint_namedBySpan_Response';

interface Captured {
  records: Map<string, CaptureAliasRecord>;
  surface: string;
}

describe("the capture's line fallback never reads a declaration's name (#1785)", () => {
  let repoDir: string;
  let captured: Captured;

  before(() => {
    // Realpath: macOS's tmpdir is a symlink, and the compiler realpaths
    // some resolutions but not others.
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1785-capture-')));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, target: 'es2022', module: 'esnext', moduleResolution: 'bundler' },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'money.ts'), MONEY_TS);
    fs.writeFileSync(path.join(repoDir, SOURCE_REL), LEDGER_TS);

    const summaryLine = 'export type LedgerSummary = { total: number; currency: Currency };';
    const memberAt = LEDGER_TS.indexOf(summaryLine) + summaryLine.indexOf('total: number');
    const nameAt = LEDGER_TS.indexOf(summaryLine) + summaryLine.indexOf('LedgerSummary');

    const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1785-out-'));
    const result = captureStub({
      repoRoot: repoDir,
      serviceName: 'line-declaration-name',
      outDir,
      anchors: [
        ...Object.entries({ ...DECLARATION_LINES, ...VALUE_LINES }).map(([alias, [prefix]]) => ({
          kind: 'infer' as const,
          alias,
          source_file: SOURCE_REL,
          anchor_origin: 'deterministic-infer' as const,
          // LINE ONLY, as a consumer call-result locator arrives.
          line_number: lineOf(prefix),
        })),
        {
          kind: 'infer' as const,
          alias: PARTIAL_SPAN_ALIAS,
          source_file: SOURCE_REL,
          anchor_origin: 'deterministic-infer' as const,
          // A span that covers no node (half a member), so the locator falls
          // back to the line: the #1775 route, on a request that is NOT
          // line-only.
          span_start: memberAt + 2,
          span_end: memberAt + 'total: num'.length,
          line_number: lineOf('export type LedgerSummary'),
        },
        {
          kind: 'infer' as const,
          alias: NAMED_BY_SPAN_ALIAS,
          source_file: SOURCE_REL,
          anchor_origin: 'deterministic-infer' as const,
          // A span over the name itself: the scanner named this node.
          span_start: nameAt,
          span_end: nameAt + 'LedgerSummary'.length,
          line_number: lineOf('export type LedgerSummary'),
        },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    captured = {
      records: new Map(result.aliases.map((r) => [r.alias, r])),
      surface: fs.readFileSync(path.join(outDir, 'types/surface.d.ts'), 'utf-8'),
    };
    fs.rmSync(outDir, { recursive: true, force: true });
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** The whole declaration of `alias` in the surface, whitespace collapsed. */
  function declared(alias: string): string {
    const start = captured.surface.indexOf(`export type ${alias} =`);
    assert.ok(start >= 0, `surface must declare ${alias}:\n${captured.surface}`);
    const next = captured.surface.indexOf('export type ', start + 1);
    return captured.surface
      .slice(start, next < 0 ? undefined : next)
      .replace(/\s+/g, ' ')
      .trim();
  }

  for (const [alias, [prefix, kind]] of Object.entries(DECLARATION_LINES)) {
    it(`abstains on a ${kind} line (\`${prefix.trim()}\`)`, () => {
      assert.strictEqual(
        declared(alias),
        `export type ${alias} = unknown;`,
        `a declaration's line names no payload`
      );
      const record = captured.records.get(alias);
      assert.ok(record, `no record for ${alias}`);
      assert.strictEqual(record.serialization, 'structural_fallback');
      // An abstain, not a demotion: nothing for the backfill to re-anchor.
      assert.strictEqual(record.capture_failure_reason, undefined);
      const reason = record.self_check_detail ?? '';
      assert.match(
        reason,
        new RegExp(`name of ${kind} at src/ledger\\.ts:${lineOf(prefix)} `),
        `the reason must name the declaration the line resolved: ${reason}`
      );
      assert.ok(!reason.includes(repoDir), `the reason must not carry an absolute path: ${reason}`);
    });
  }

  it("does not walk on to a function's parameter", () => {
    // The node after a function's name on its line is its first parameter.
    // Reading that instead would publish `LedgerFilter` as the response.
    for (const alias of ['Endpoint_function_Response', 'Endpoint_method_Response']) {
      assert.ok(
        !/LedgerFilter|kind/.test(declared(alias)),
        `${alias} read the parameter: ${declared(alias)}`
      );
    }
  });

  it('abstains when a span that names nothing falls back to a declaration line', () => {
    assert.strictEqual(declared(PARTIAL_SPAN_ALIAS), `export type ${PARTIAL_SPAN_ALIAS} = unknown;`);
    assert.strictEqual(captured.records.get(PARTIAL_SPAN_ALIAS)?.capture_failure_reason, undefined);
  });

  it('honours a span that names a declaration: only the line fallback abstains', () => {
    // A span, like an expression text, is the scanner naming a node, and the
    // type at it is a located fact (carrick#766). The rule is about what a
    // line alone can name.
    const record = captured.records.get(NAMED_BY_SPAN_ALIAS);
    assert.strictEqual(record?.serialization, 'node_builder', record?.self_check_detail);
    assert.match(declared(NAMED_BY_SPAN_ALIAS), /total: number/);
  });

  for (const [alias, [prefix, member]] of Object.entries(VALUE_LINES)) {
    it(`keeps resolving a value on its line (\`${prefix.trim()}\`)`, () => {
      const record = captured.records.get(alias);
      assert.ok(record, `no record for ${alias}`);
      assert.strictEqual(record.serialization, 'node_builder', `${alias}: ${record.self_check_detail}`);
      assert.match(declared(alias), member);
    });
  }
});
