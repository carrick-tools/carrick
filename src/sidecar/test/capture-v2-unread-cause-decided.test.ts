/**
 * carrick#3995 T2: the capture says why an alias it read is untyped, at the
 * place it decided it.
 *
 * - A demotion whose type is framework machinery says `machinery_envelope`.
 * - Every other demotion (a file outside the program, a symbol that is not an
 *   export, an anchor the surface would emit under the wrong options) says
 *   `site_unreadable`: the scan named a site, and no type could be read there.
 * - An abstain because the anchor named no payload (a line that declares
 *   something, or a line whose first node is a bare top type) says
 *   `not_located`.
 * - An abstain the capture decided from the code (a response table with no
 *   success body, a result label the type cannot satisfy) carries no cause:
 *   the reason is stated elsewhere.
 *
 * The word rides the alias's capture record as `unread_cause`. Only the index
 * reads it; no verdict does, and it is not part of `surface_hash`.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord, CaptureAnchorRequest } from '../src/capture/api.js';

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

const PARCELS_TS = `export interface Parcel { id: string; weight: number }
declare function loadParcel(): Promise<Parcel>;
declare const untypedRow: any;

export async function parcelReply() {
  return new Response(JSON.stringify(await loadParcel()));
}

export async function parcelBody(): Promise<Parcel> {
  return loadParcel();
}

const internalParcel = { id: 'p1', weight: 2 };
export const shelfCount = internalParcel.weight;

export { parcelBody as fetchParcel };

export function readRow() {
  return untypedRow;
}

export type StatusTable = { 404: { message: string } };
`;

function lineOf(text: string, prefix: string): number {
  const lines = text.split('\n');
  const hits = lines.map((l, i) => (l.startsWith(prefix) ? i + 1 : 0)).filter((n) => n > 0);
  assert.strictEqual(hits.length, 1, `fixture must hold exactly one line starting with: ${prefix}`);
  return hits[0];
}

function writeRepo(files: Record<string, string>): string {
  const repoRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3995-t2-'));
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
  anchors: CaptureAnchorRequest[],
  tsconfigPath?: string
): Map<string, CaptureAliasRecord> {
  const outDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-3995-t2-stub-'));
  roots.push(outDir);
  const result = captureStub({
    repoRoot,
    serviceName: 'parcels',
    outDir,
    anchors,
    ...(tsconfigPath ? { tsconfigPath } : {}),
  });
  assert.strictEqual(result.success, true, JSON.stringify(result.errors));
  return new Map(result.aliases.map((record) => [record.alias, record]));
}

function record(records: Map<string, CaptureAliasRecord>, alias: string): CaptureAliasRecord {
  const found = records.get(alias);
  assert.ok(found, `no record for ${alias}`);
  return found!;
}

describe('capture: an untyped alias carries the cause the capture decided (carrick#3995 T2)', () => {
  const repoRoot = writeRepo({
    'tsconfig.json': JSON.stringify(TSCONFIG),
    'src/parcels.ts': PARCELS_TS,
    // A route file the program does not hold (outside `include`).
    'tools/legacy-route.mjs': 'export function legacyReply() { return { ok: true }; }\n',
  });
  const records = capture(repoRoot, [
    {
      kind: 'infer',
      alias: 'Parcel_Reply_Response',
      source_file: 'src/parcels.ts',
      anchor_origin: 'deterministic-infer',
      expression_text: 'new Response(JSON.stringify(await loadParcel()))',
      unwrap: 'none',
    },
    {
      kind: 'infer',
      alias: 'Legacy_Reply_Response',
      source_file: 'tools/legacy-route.mjs',
      anchor_origin: 'deterministic-infer',
      line_number: 1,
    },
    {
      kind: 'symbol',
      alias: 'Internal_Parcel_Response',
      source_file: 'src/parcels.ts',
      symbol_name: 'internalParcel',
      anchor_origin: 'llm-symbol',
    },
    {
      kind: 'infer',
      alias: 'Fetch_Parcel_Response',
      source_file: 'src/parcels.ts',
      anchor_origin: 'deterministic-infer',
      line_number: lineOf(PARCELS_TS, 'export { parcelBody as fetchParcel }'),
    },
    {
      kind: 'infer',
      alias: 'Row_Response',
      source_file: 'src/parcels.ts',
      anchor_origin: 'deterministic-infer',
      line_number: lineOf(PARCELS_TS, '  return untypedRow;'),
    },
    {
      kind: 'symbol',
      alias: 'Status_Table_Response',
      source_file: 'src/parcels.ts',
      symbol_name: 'StatusTable',
      anchor_origin: 'llm-symbol',
      consumer_response: true,
    },
    {
      kind: 'infer',
      alias: 'Parcel_Body_Response',
      source_file: 'src/parcels.ts',
      anchor_origin: 'deterministic-infer',
      expression_text: 'loadParcel()',
    },
  ]);

  it('a handler returning a built Response says machinery_envelope', () => {
    const r = record(records, 'Parcel_Reply_Response');
    assert.match(r.capture_failure_reason ?? '', /framework machinery/);
    assert.strictEqual(r.unread_cause, 'machinery_envelope');
  });

  it('a route file outside the program says site_unreadable', () => {
    const r = record(records, 'Legacy_Reply_Response');
    assert.match(r.capture_failure_reason ?? '', /source file not in program/);
    assert.strictEqual(r.unread_cause, 'site_unreadable');
  });

  it('a symbol anchor naming a non-export says site_unreadable', () => {
    const r = record(records, 'Internal_Parcel_Response');
    assert.match(r.capture_failure_reason ?? '', /is not an export of/);
    assert.strictEqual(r.unread_cause, 'site_unreadable');
  });

  it('an `export { ... }` line says not_located', () => {
    const r = record(records, 'Fetch_Parcel_Response');
    assert.strictEqual(r.capture_failure_reason, undefined);
    assert.match(r.self_check_detail ?? '', /declaration's name is not a payload/);
    assert.strictEqual(r.unread_cause, 'not_located');
  });

  it('a line whose first node is a bare top type says not_located', () => {
    const r = record(records, 'Row_Response');
    assert.strictEqual(r.capture_failure_reason, undefined);
    assert.match(r.self_check_detail ?? '', /line-only locator resolved/);
    assert.strictEqual(r.unread_cause, 'not_located');
  });

  it('a decided abstain (a status table with no success body) carries no cause', () => {
    const r = record(records, 'Status_Table_Response');
    assert.match(r.self_check_detail ?? '', /no success row/);
    assert.strictEqual(r.unread_cause, undefined);
  });

  it('a typed alias carries no cause', () => {
    const r = record(records, 'Parcel_Body_Response');
    assert.strictEqual(r.capture_failure_reason, undefined);
    assert.strictEqual(r.unread_cause, undefined);
  });
});

describe('capture: a re-anchor demotion says site_unreadable (carrick#3995 T2)', () => {
  it('an anchor typed under a project whose options differ from the emit project', () => {
    const repoRoot = writeRepo({
      'tsconfig.json': JSON.stringify({
        files: [],
        references: [{ path: './tsconfig.src.json' }, { path: './tsconfig.jobs.json' }],
      }),
      'tsconfig.src.json': JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', skipLibCheck: true },
        include: ['src'],
      }),
      'tsconfig.jobs.json': JSON.stringify({
        compilerOptions: { strict: false, module: 'commonjs', skipLibCheck: true },
        include: ['jobs'],
      }),
      'src/shelf.ts': 'export interface Shelf { id: string }\nexport interface Bin { code: string }\n',
      'jobs/sweep.ts': 'export interface SweepResult { swept: number }\n',
    });
    const records = capture(
      repoRoot,
      [
        { kind: 'symbol', alias: 'Shelf_Response', source_file: 'src/shelf.ts', symbol_name: 'Shelf', anchor_origin: 'llm-symbol' },
        { kind: 'symbol', alias: 'Bin_Response', source_file: 'src/shelf.ts', symbol_name: 'Bin', anchor_origin: 'llm-symbol' },
        { kind: 'symbol', alias: 'Sweep_Response', source_file: 'jobs/sweep.ts', symbol_name: 'SweepResult', anchor_origin: 'llm-symbol' },
      ],
      'tsconfig.json'
    );
    const sweep = record(records, 'Sweep_Response');
    assert.match(sweep.capture_failure_reason ?? '', /the project that owns its file, whose options differ/);
    assert.strictEqual(sweep.unread_cause, 'site_unreadable');
    assert.strictEqual(record(records, 'Shelf_Response').unread_cause, undefined);
  });
});
