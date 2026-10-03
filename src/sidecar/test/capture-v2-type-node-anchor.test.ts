/**
 * carrick#1775: an infer anchor whose span covers exactly a TYPE node serves
 * that type, never the declaration around it.
 *
 * The scanner points an `Expression` request at the property type a generated
 * GraphQL document declaration states for a root field (carrick#1761), so the
 * span names a type node inside a type alias, not a value. The capture's span
 * locator accepted value-space nodes only: no node covered the span, the
 * locator fell back to the first "expression" on the line, which on a one-line
 * alias is the alias's own NAME, and the alias served the whole operation
 * result under a field-keyed row.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';

const GENERATED_TS = `export type LedgerQuery = { __typename: 'Query', ledger?: { __typename: 'Ledger', id: string, entries?: unknown | null } | null };
export type InvoiceQuery = { __typename: 'Query', invoice?: { __typename: 'Invoice', id: string, total: number } | null };
`;

const LEDGER_FIELD = "{ __typename: 'Ledger', id: string, entries?: unknown | null } | null";
const INVOICE_FIELD = "{ __typename: 'Invoice', id: string, total: number }";

describe('capture places an infer anchor on a type node its span covers exactly (#1775)', () => {
  let repoDir: string;

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1775-capture-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, target: 'es2022', module: 'esnext', moduleResolution: 'bundler' },
        include: ['src'],
      })
    );
    fs.writeFileSync(path.join(repoDir, 'src', 'generated.ts'), GENERATED_TS);
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  function spanOf(text: string): { span_start: number; span_end: number; line_number: number } {
    const at = GENERATED_TS.indexOf(text);
    assert.ok(at >= 0, `fixture must contain: ${text}`);
    assert.strictEqual(GENERATED_TS.indexOf(text, at + 1), -1, `fixture text must be unique: ${text}`);
    return {
      span_start: at,
      span_end: at + text.length,
      line_number: GENERATED_TS.slice(0, at).split('\n').length,
    };
  }

  it('serves the property type, not the operation alias around it', () => {
    const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1775-out-'));
    try {
      const result = captureStub({
        repoRoot: repoDir,
        serviceName: 'type-node-anchor',
        outDir,
        anchors: [
          {
            kind: 'infer',
            alias: 'Endpoint_ledger_Response',
            source_file: 'src/generated.ts',
            anchor_origin: 'deterministic-infer',
            ...spanOf(LEDGER_FIELD),
          },
          {
            kind: 'infer',
            alias: 'Endpoint_invoice_Response',
            source_file: 'src/generated.ts',
            anchor_origin: 'deterministic-infer',
            // The object inside the union: a type node nested in another one.
            ...spanOf(INVOICE_FIELD),
          },
        ],
      });
      assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
      const surface = fs.readFileSync(path.join(outDir, 'types/surface.d.ts'), 'utf-8');
      /** The whole declaration of `alias`, whitespace collapsed. */
      const declared = (alias: string): string => {
        const start = surface.indexOf(`export type ${alias} =`);
        assert.ok(start >= 0, `surface must declare ${alias}:\n${surface}`);
        const next = surface.indexOf('export type ', start + 1);
        return surface
          .slice(start, next < 0 ? undefined : next)
          .replace(/\s+/g, ' ')
          .trim();
      };

      const ledger = declared('Endpoint_ledger_Response');
      assert.ok(!/"Query"|ledger\??:/.test(ledger), `the operation is not the field: ${ledger}`);
      assert.match(ledger, /__typename: "Ledger"/);
      assert.match(ledger, /entries\?: unknown/);
      assert.match(ledger, /\| null;$/, `nullable as declared: ${ledger}`);

      const invoice = declared('Endpoint_invoice_Response');
      assert.ok(!/"Query"|invoice\??:|null/.test(invoice), `exactly the object node: ${invoice}`);
      assert.match(invoice, /total: number/);

      const ledgerRecord = result.aliases.find((a) => a.alias === 'Endpoint_ledger_Response');
      assert.strictEqual(ledgerRecord?.serialization, 'node_builder');
      // The member the declaration writes `unknown` is the declaration's own.
      assert.deepStrictEqual(
        ledgerRecord?.any_provenance?.map((p) => [p.path, p.reason]),
        [['entries', 'declared']]
      );
    } finally {
      fs.rmSync(outDir, { recursive: true, force: true });
    }
  });

  it('reads a type node only when the span covers it exactly', () => {
    const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1775-out-'));
    try {
      // Half a member: no type node starts and ends there.
      const field = spanOf(LEDGER_FIELD);
      const result = captureStub({
        repoRoot: repoDir,
        serviceName: 'type-node-anchor',
        outDir,
        anchors: [
          {
            kind: 'infer',
            alias: 'Endpoint_partial_Response',
            source_file: 'src/generated.ts',
            anchor_origin: 'deterministic-infer',
            span_start: field.span_start + 2,
            span_end: field.span_end - 3,
            line_number: field.line_number,
          },
        ],
      });
      assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
      const surface = fs.readFileSync(path.join(outDir, 'types/surface.d.ts'), 'utf-8');
      assert.ok(
        !/export type Endpoint_partial_Response = \{\s*__typename: "Ledger"/.test(surface),
        `a span that names no node is not read as the field:\n${surface}`
      );
    } finally {
      fs.rmSync(outDir, { recursive: true, force: true });
    }
  });
});
