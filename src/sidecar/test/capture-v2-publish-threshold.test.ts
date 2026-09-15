/**
 * What the capture record must say for the scanner to decide whether an
 * alias's answer is worth publishing (carrick#1165).
 *
 * The scanner publishes the capture's printed answer as an operation's type
 * and counts the operation typed. Two answers print a concrete-looking shape
 * that names something the stub tree cannot resolve:
 *
 *  - literal text (printed upstream) or an anonymous print reusing a source
 *    annotation whose import did not resolve, naming an identifier nothing
 *    declares;
 *  - an emitted declaration whose own module imports a module that does not
 *    exist on the scanned checkout.
 *
 * Both used to reach the record only as prose (or not at all), so nothing
 * downstream could refuse them. The record now carries them as data:
 * `undeclared_names` and `dangling_specifiers`.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord } from '../src/capture/api.js';

const ROUTE = 'src/parcels/route.ts';
const VIEWS = 'src/parcels/views.ts';
const CLEAN = 'src/parcels/clean.ts';

const FILES: Record<string, string> = {
  'tsconfig.json': JSON.stringify({
    compilerOptions: {
      target: 'ES2022',
      module: 'ESNext',
      moduleResolution: 'Bundler',
      strict: true,
      skipLibCheck: true,
      rootDir: 'src',
    },
    include: ['src'],
  }),
  // The generated module both files import was never generated.
  [ROUTE]: [
    "import type { ParcelRow } from '../generated/client';",
    '',
    'declare function loadRow(id: string): Promise<ParcelRow>;',
    '',
    'export async function readParcel(id: string) {',
    '  const parcel: ParcelRow = await loadRow(id);',
    '  const answer = { id, parcel };',
    '  return answer;',
    '}',
    '',
    'export function readFlags(input: { sendEmail?: boolean; archived: boolean | null }) {',
    '  const flags = { sendEmail: input.sendEmail, archived: input.archived, stamp: new Date() };',
    '  return flags;',
    '}',
    '',
  ].join('\n'),
  [VIEWS]: [
    "import type { ParcelRow } from '../generated/client';",
    '',
    'export interface ParcelView {',
    '  id: string;',
    '  row: ParcelRow;',
    '}',
    '',
    'export interface SiblingView {',
    '  id: string;',
    '}',
    '',
  ].join('\n'),
  [CLEAN]: [
    'export interface CleanView {',
    '  id: string;',
    '  sendEmail?: boolean;',
    '}',
    '',
  ].join('\n'),
};

function lineOf(source: string, text: string): number {
  const at = source.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  return source.slice(0, at).split('\n').length;
}

describe('capture record carries what an answer cannot resolve (#1165)', () => {
  let root: string;
  let records: Map<string, CaptureAliasRecord>;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-publish-threshold-'));
    const repoRoot = path.join(root, 'repo');
    for (const [rel, text] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(repoRoot, rel)), { recursive: true });
      fs.writeFileSync(path.join(repoRoot, rel), text);
    }
    const route = FILES[ROUTE];
    const result = captureStub({
      repoRoot,
      serviceName: 'publish-threshold',
      outDir: path.join(root, 'stub'),
      anchors: [
        {
          kind: 'infer',
          alias: 'Endpoint_answer_Response',
          source_file: ROUTE,
          anchor_origin: 'deterministic-infer',
          line_number: lineOf(route, 'return answer;'),
          expression_text: 'answer',
        },
        {
          kind: 'infer',
          alias: 'Endpoint_flags_Response',
          source_file: ROUTE,
          anchor_origin: 'deterministic-infer',
          line_number: lineOf(route, 'return flags;'),
          expression_text: 'flags',
        },
        {
          kind: 'literal',
          alias: 'Endpoint_literal_Response',
          type_text: '{ manifest: ManifestRow; issued: Date; items: Array<LineRow>; }',
          anchor_origin: 'deterministic-infer',
        },
        {
          kind: 'literal',
          alias: 'Endpoint_generic_Response',
          type_text: '{ pick: <T>(items: T[]) => T; keys: { [K in "a" | "b"]: K }; }',
          anchor_origin: 'deterministic-infer',
        },
        {
          kind: 'symbol',
          alias: 'Endpoint_view_Response',
          source_file: VIEWS,
          symbol_name: 'ParcelView',
          anchor_origin: 'llm-symbol',
        },
        {
          kind: 'symbol',
          alias: 'Endpoint_sibling_Response',
          source_file: VIEWS,
          symbol_name: 'SiblingView',
          anchor_origin: 'llm-symbol',
        },
        {
          kind: 'symbol',
          alias: 'Endpoint_clean_Response',
          source_file: CLEAN,
          symbol_name: 'CleanView',
          anchor_origin: 'llm-symbol',
        },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    records = new Map(result.aliases.map((record) => [record.alias, record]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('records an identifier the anonymous print names but nothing declares', () => {
    const record = records.get('Endpoint_answer_Response');
    assert.deepStrictEqual(record?.undeclared_names, ['ParcelRow'], JSON.stringify(record));
  });

  it('records the identifiers literal text names that nothing declares, not the globals', () => {
    const record = records.get('Endpoint_literal_Response');
    assert.deepStrictEqual(
      record?.undeclared_names,
      ['LineRow', 'ManifestRow'],
      JSON.stringify(record)
    );
  });

  it('records the module an emitted declaration imports that does not exist', () => {
    const record = records.get('Endpoint_view_Response');
    assert.deepStrictEqual(
      record?.dangling_specifiers,
      ['../generated/client'],
      JSON.stringify(record)
    );
  });

  it('blames a declaration file, not a declaration: a clean sibling in that file is listed too', () => {
    // The self-check attributes a failed import to every alias whose closure
    // reaches the FILE that holds it (fail closed), and has since before this
    // field existed: the sibling already self-checks `decayed_internal`. The
    // field states that verdict's cause as data; it does not narrow it.
    const record = records.get('Endpoint_sibling_Response');
    assert.strictEqual(record?.self_check, 'decayed_internal', JSON.stringify(record));
    assert.deepStrictEqual(record?.dangling_specifiers, ['../generated/client']);
  });

  it('records neither for aliases that resolve, globals included', () => {
    for (const alias of [
      'Endpoint_flags_Response',
      'Endpoint_clean_Response',
      'Endpoint_generic_Response',
    ]) {
      const record = records.get(alias);
      assert.strictEqual(record?.undeclared_names, undefined, JSON.stringify(record));
      assert.strictEqual(record?.dangling_specifiers, undefined, JSON.stringify(record));
    }
  });
});
