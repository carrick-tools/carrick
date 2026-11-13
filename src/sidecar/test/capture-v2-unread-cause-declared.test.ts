/**
 * carrick#4046: a site the capture located whose type is itself a bare
 * `any` or `unknown` says `declared`, the word the deep walk already uses for
 * a top type the source states. It was left with no cause and read
 * `not_recorded` in the index.
 *
 * - A located symbol whose declared type is `any` or `unknown` says `declared`.
 * - A located symbol with a concrete type carries no cause.
 * - An abstain decided from the code keeps its own reason: a response table
 *   with no success row says `no_success_payload`; a result label the type
 *   cannot satisfy stays without a cause (it is not a top type the source
 *   declares).
 * - A cause the capture already decided is never replaced.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type {
  CaptureAliasRecord,
  CaptureAnchorRequest,
  SymbolAnchorRequest,
} from '../src/capture/api.js';

const roots: string[] = [];

after(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

const TSCONFIG = {
  compilerOptions: {
    strict: true,
    module: 'esnext',
    moduleResolution: 'bundler',
    target: 'es2022',
    lib: ['es2022', 'dom'],
    skipLibCheck: true,
  },
  include: ['src'],
};

const SHELVES_TS = `export interface Shelf { id: string; slots: number }
export type LooseShelf = any;
export type OpenShelf = unknown;
export type StatusTable = { 404: { message: string } };
declare const untypedShelf: any;

export function readShelf() {
  return untypedShelf;
}
`;

function writeRepo(files: Record<string, string>): string {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4046-'));
  roots.push(repoRoot);
  for (const [rel, text] of Object.entries(files)) {
    const abs = path.join(repoRoot, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, text);
  }
  return repoRoot;
}

function capture(
  repoRoot: string,
  anchors: CaptureAnchorRequest[]
): Map<string, CaptureAliasRecord> {
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-4046-stub-'));
  roots.push(outDir);
  const result = captureStub({ repoRoot, serviceName: 'shelves', outDir, anchors });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return new Map(result.aliases.map((record) => [record.alias, record]));
}

function record(records: Map<string, CaptureAliasRecord>, alias: string): CaptureAliasRecord {
  const found = records.get(alias);
  assert.ok(found, `no record for ${alias}`);
  return found!;
}

describe('capture: a located site whose type is a bare top type says declared (carrick#4046)', () => {
  const repoRoot = writeRepo({
    'tsconfig.json': JSON.stringify(TSCONFIG),
    'src/shelves.ts': SHELVES_TS,
  });
  const symbol = (alias: string, name: string): SymbolAnchorRequest => ({
    kind: 'symbol',
    alias,
    source_file: 'src/shelves.ts',
    symbol_name: name,
    anchor_origin: 'llm-symbol',
  });
  const records = capture(repoRoot, [
    symbol('Loose_Response', 'LooseShelf'),
    symbol('Open_Response', 'OpenShelf'),
    symbol('Shelf_Response', 'Shelf'),
    { ...symbol('Status_Table_Response', 'StatusTable'), consumer_response: true },
  ]);

  it('a symbol declared any says declared', () => {
    const r = record(records, 'Loose_Response');
    assert.strictEqual(r.top_type_at_self_check, true);
    assert.strictEqual(r.unread_cause, 'declared');
  });

  it('a symbol declared unknown says declared', () => {
    const r = record(records, 'Open_Response');
    assert.strictEqual(r.top_type_at_self_check, true);
    assert.strictEqual(r.unread_cause, 'declared');
  });

  it('a concrete type carries no cause', () => {
    const r = record(records, 'Shelf_Response');
    assert.strictEqual(r.top_type_at_self_check, false);
    assert.strictEqual(r.unread_cause, undefined);
  });

  it('a response table with no success row says no_success_payload, not declared', () => {
    const r = record(records, 'Status_Table_Response');
    assert.match(r.self_check_detail ?? '', /no success row/);
    assert.strictEqual(r.unread_cause, 'no_success_payload');
  });

  it('a cause the capture already decided is not replaced', () => {
    const withLine = capture(repoRoot, [
      {
        kind: 'infer',
        alias: 'Row_Response',
        source_file: 'src/shelves.ts',
        anchor_origin: 'deterministic-infer',
        line_number: 8,
      },
    ]);
    const r = record(withLine, 'Row_Response');
    assert.strictEqual(r.unread_cause, 'not_located');
  });
});
