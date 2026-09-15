/**
 * An `any` the compiler printed because an import did not resolve is not an
 * `any` the source declares (carrick#1164).
 *
 * The node builder prints TypeScript's unresolved-reference placeholder (the
 * `error` intrinsic) as the keyword `any`. Once printed, the emitted surface
 * holds the same text an author annotation would, and the self-check — which
 * reads emitted text — reported every such member as `declared`: "declared that
 * way in the source". A reader told that stops looking, where the truth is that
 * a dependency or a generated module was missing on the scanned checkout, and
 * the fix is to install or generate it.
 *
 * The two causes are only distinguishable on the SOURCE program, where the
 * member's type is still the placeholder. The capture reads them there, at
 * anchor time, and the self-check labels the matching findings.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord, TypeProvenance } from '../src/capture/api.js';

const ROUTE = 'src/tasks/route.ts';

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
  // The generated client this module imports was never generated.
  'src/db/client.ts': [
    "import { GeneratedClient } from './generated/client';",
    'export const db = new GeneratedClient();',
    '',
  ].join('\n'),
  [ROUTE]: [
    "import { db } from '../db/client';",
    '',
    "type Status = 'open' | 'done';",
    '',
    'export async function loadTask(id: string) {',
    '  const row = await db.task.findUnique({ where: { id } });',
    "  const status: Status = 'open';",
    '  const loaded = { id: row.id, title: row.title, status };',
    '  return loaded;',
    '}',
    '',
    'export type LoadedTask = Awaited<ReturnType<typeof loadTask>>;',
    '',
    'export function describeTask() {',
    "  const described: { id: any; title: string } = { id: 1, title: 'x' };",
    '  return described;',
    '}',
    '',
  ].join('\n'),
};

function lineOf(source: string, text: string): number {
  const at = source.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  return source.slice(0, at).split('\n').length;
}

function findingAt(record: CaptureAliasRecord | undefined, member: string): TypeProvenance {
  const finding = record?.any_provenance?.find((entry) => entry.path === member);
  assert.ok(finding, `no finding at '${member}': ${JSON.stringify(record)}`);
  return finding;
}

describe('any_provenance separates an unresolved import from a declared any (#1164)', () => {
  let root: string;
  let records: Map<string, CaptureAliasRecord>;

  before(() => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-unresolved-import-'));
    const repoRoot = path.join(root, 'repo');
    for (const [rel, text] of Object.entries(FILES)) {
      fs.mkdirSync(path.dirname(path.join(repoRoot, rel)), { recursive: true });
      fs.writeFileSync(path.join(repoRoot, rel), text);
    }
    const route = FILES[ROUTE];
    const result = captureStub({
      repoRoot,
      serviceName: 'unresolved-import',
      outDir: path.join(root, 'stub'),
      anchors: [
        {
          kind: 'infer',
          alias: 'Endpoint_loaded_Response',
          source_file: ROUTE,
          anchor_origin: 'deterministic-infer',
          line_number: lineOf(route, 'return loaded;'),
          expression_text: 'loaded',
        },
        {
          kind: 'infer',
          alias: 'Endpoint_described_Response',
          source_file: ROUTE,
          anchor_origin: 'deterministic-infer',
          line_number: lineOf(route, 'return described;'),
          expression_text: 'described',
        },
        {
          kind: 'symbol',
          alias: 'Endpoint_symbol_Response',
          source_file: ROUTE,
          symbol_name: 'LoadedTask',
          anchor_origin: 'llm-symbol',
          array_depth: 1,
        },
      ],
    });
    assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
    records = new Map(result.aliases.map((record) => [record.alias, record]));
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('labels a member typed through the missing module as an unresolved import', () => {
    const finding = findingAt(records.get('Endpoint_loaded_Response'), 'id');
    assert.strictEqual(finding.kind, 'any');
    assert.strictEqual(finding.reason, 'unresolved_import', JSON.stringify(finding));
  });

  it('labels the same member through a symbol anchor, under its array depth', () => {
    const finding = findingAt(records.get('Endpoint_symbol_Response'), '<0>.id');
    assert.strictEqual(finding.reason, 'unresolved_import', JSON.stringify(finding));
  });

  it('names the module that did not resolve, as the source wrote it', () => {
    const detail = findingAt(records.get('Endpoint_loaded_Response'), 'id').detail ?? '';
    assert.match(detail, /'\.\/generated\/client'/, detail);
    assert.ok(!detail.includes(root), `the detail must not carry an absolute path: ${detail}`);
  });

  it('keeps a resolved member out of the findings', () => {
    const record = records.get('Endpoint_loaded_Response');
    assert.ok(
      !record?.any_provenance?.some((entry) => entry.path === 'status'),
      JSON.stringify(record?.any_provenance)
    );
  });

  it('still labels an author-written any as declared', () => {
    const finding = findingAt(records.get('Endpoint_described_Response'), 'id');
    assert.strictEqual(finding.reason, 'declared', JSON.stringify(finding));
  });
});
