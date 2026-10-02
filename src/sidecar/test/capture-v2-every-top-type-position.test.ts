/**
 * The capture's deep walk names EVERY position a top type sits at
 * (carrick#1752).
 *
 * `any` and `unknown` are intrinsic types: the compiler hands out one object
 * for each, however many members are typed with it. The walk's cycle set
 * remembers types it has met, so it used to report the first `unknown` and
 * the first `any` it reached and pass over every later position silently.
 *
 * That hid a cause. A member whose import the scanned checkout cannot resolve
 * is rewritten to `unknown` in the emitted declaration (carrick#1397), and the
 * record labels it `unresolved_import`. A member the source itself declares
 * `unknown` is labelled `declared`. When the declared one came first in walk
 * order, the rewritten one was never reported, and the record read as if every
 * top type in the type were the author's own. The scanner now trusts that
 * record to decide whether an open member leaves a typed contract, so the
 * record has to name every position.
 *
 * The first finding is unchanged by this: it is still the first position in
 * walk order, and it is what the check phase pre-gates on.
 *
 * An OBJECT type met twice (one interface typing two members) is still walked
 * once: its members are one declaration, so whatever cause a top type inside
 * it has, the first path already names it.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord } from '../src/capture/api.js';

const VIEW = 'src/tasks/view.ts';

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
  // The generated module was never generated. `notes` is the author's own
  // open member and comes first; `row` is typed through the missing module.
  [VIEW]: [
    "import type { TaskRow } from '../generated/client';",
    '',
    'export interface TaskView {',
    '  id: string;',
    '  notes: unknown;',
    '  row: TaskRow;',
    '}',
    '',
  ].join('\n'),
};

type Finding = [path: string, kind: string, reason: string];

function findings(record: CaptureAliasRecord | undefined): Finding[] {
  return (record?.any_provenance ?? []).map((entry) => [entry.path, entry.kind, entry.reason]);
}

describe('capture record names every top-type position (#1752)', () => {
  let root: string;
  let records: Map<string, CaptureAliasRecord>;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-every-position-'));
    const repoRoot = path.join(root, 'repo');
    for (const [rel, text] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(repoRoot, rel)), { recursive: true });
      fs.writeFileSync(path.join(repoRoot, rel), text);
    }
    const result = captureStub({
      repoRoot,
      serviceName: 'every-position',
      outDir: path.join(root, 'stub'),
      anchors: [
        {
          kind: 'literal',
          alias: 'Endpoint_repeated_Response',
          type_text:
            '{ a: unknown; b: unknown; c: { d: unknown; }; e: any; f: any; g: string; }',
          anchor_origin: 'deterministic-infer',
        },
        {
          kind: 'literal',
          alias: 'Endpoint_union_Response',
          type_text: '{ v: { x: unknown; } | { x: unknown; y: string; }; }',
          anchor_origin: 'deterministic-infer',
        },
        {
          kind: 'literal',
          alias: 'Endpoint_lists_Response',
          type_text: '{ tags: unknown[]; }',
          anchor_origin: 'deterministic-infer',
        },
        {
          kind: 'symbol',
          alias: 'Endpoint_view_Response',
          source_file: VIEW,
          symbol_name: 'TaskView',
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

  it('reports a top type at every member that holds one, not once per kind', () => {
    assert.deepStrictEqual(findings(records.get('Endpoint_repeated_Response')), [
      ['a', 'unknown', 'declared'],
      ['b', 'unknown', 'declared'],
      ['c.d', 'unknown', 'declared'],
      ['e', 'any', 'declared'],
      ['f', 'any', 'declared'],
    ]);
  });

  it('reports a position reached through two union members once', () => {
    assert.deepStrictEqual(findings(records.get('Endpoint_union_Response')), [
      ['v.x', 'unknown', 'declared'],
    ]);
  });

  it("names an array's element once, not again as its number index", () => {
    assert.deepStrictEqual(findings(records.get('Endpoint_lists_Response')), [
      ['tags<0>', 'unknown', 'declared'],
    ]);
  });

  it('does not let a declared open member hide one an unresolved import left', () => {
    const record = records.get('Endpoint_view_Response');
    assert.strictEqual(record?.dangling_specifiers, undefined, JSON.stringify(record));
    assert.deepStrictEqual(findings(record), [
      ['notes', 'unknown', 'declared'],
      ['row', 'unknown', 'unresolved_import'],
    ]);
  });
});
