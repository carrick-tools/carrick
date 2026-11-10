/**
 * carrick#1910: in the position rows' process, a package of the checkout
 * whose manifest names build output that was never built is read from its
 * source, whether or not the service config references it.
 *
 * A cloud scan installs a monorepo and never builds it, so a workspace
 * package's `typings` (or `types`, or an `exports` entry) names a file under
 * `build/` or `dist/` that does not exist, and every call on a value it types
 * reads the error type. The rows' process is told it is that process by the
 * `init` field `workspace_source`; only then does its loader seed carrick#2418's
 * reference redirect with the packages the install links into the checkout
 * (a `node_modules/<name>` whose real path is inside the checkout and under
 * no `node_modules`), each through its own `tsconfig*.json` files.
 *
 * Every other process (infer, the compat judge's anchors, capture) reads the
 * package as before: the same fixture, initialised without the field, keeps
 * the error type, and capture still reads it as the top type.
 *
 * The fixture (`test/fixtures/unbuilt-workspace/`) is materialised in a temp
 * dir with the links a workspace install writes:
 *
 *  - `@acme/ledger`: `typings` into `build/main/` (`outDir` `build/main`,
 *    `rootDir` `src`; a second `tsconfig.module.json` writes `build/module`).
 *  - `@acme/flags`: an `exports` map into `dist/esm` and `dist/cjs`, one
 *    config each.
 *  - `@acme/split`: two configs write `dist/index.d.ts` from different
 *    sources, so nothing says which one the entry is: it stays unresolved.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { captureStub } from '../src/capture/index.js';
import type { CaptureAliasRecord } from '../src/capture/api.js';
import { FLAG_ERROR_TYPE, type PositionRow } from '../src/position-rows.js';
import { SidecarClient } from './helpers.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'unbuilt-workspace');
const PACKAGES = ['ledger', 'flags', 'split'];
const HANDLER = 'services/api/src/handler.ts';
const SOURCE = fs.readFileSync(path.join(FIXTURE, HANDLER), 'utf8');

const roots: string[] = [];
after(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

interface Workspace {
  repo: string;
  service: string;
  handler: string;
  store: string;
}

/** The fixture in a temp dir, as a checkout an install linked and nothing built. */
function materialise(): Workspace {
  const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-1910-')));
  roots.push(root);
  const repo = path.join(root, 'repo');
  fs.cpSync(FIXTURE, repo, { recursive: true });
  fs.mkdirSync(path.join(repo, '.git'));
  fs.mkdirSync(path.join(repo, 'node_modules', '@acme'), { recursive: true });
  for (const name of PACKAGES) {
    fs.symlinkSync(path.join('..', '..', 'packages', name), path.join(repo, 'node_modules', '@acme', name), 'dir');
  }
  return {
    repo,
    service: path.join(repo, 'services', 'api'),
    handler: path.join(repo, HANDLER),
    store: path.join(root, 'store'),
  };
}

/** The walk's rows for the handler, from a fresh sidecar initialised with `init`. */
async function handlerRows(ws: Workspace, init: Record<string, unknown>): Promise<PositionRow[]> {
  const client = new SidecarClient();
  await client.start();
  try {
    const ready = await client.send<{ status: string; errors?: string[] }>({
      action: 'init',
      request_id: 'init',
      repo_root: ws.service,
      tsconfig_path: 'tsconfig.json',
      scan_root: ws.repo,
      ...init,
    });
    assert.strictEqual(ready.status, 'ready', JSON.stringify(ready.errors));
    const outPath = path.join(ws.store, `rows-${Object.keys(init).join('-') || 'plain'}.jsonl.gz`);
    const answer = await client.send<{ status: string; errors?: string[] }>(
      {
        action: 'position_rows',
        request_id: 'rows',
        files: [ws.handler],
        scan_root: ws.repo,
        out_path: outPath,
        service: 'api',
      },
      60_000
    );
    assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
    const lines = zlib
      .gunzipSync(fs.readFileSync(outPath))
      .toString('utf8')
      .split('\n')
      .filter((line) => line.trim() !== '')
      .map((line) => JSON.parse(line) as { f: string; r: PositionRow[] });
    const file = lines.find((line) => line.f === HANDLER);
    assert.ok(file, `no rows for the handler: ${lines.map((l) => l.f).join(', ')}`);
    return file.r;
  } finally {
    await client.stop();
  }
}

interface Inferred {
  alias: string;
  type_string: string;
  any_provenance?: unknown[];
}

/** `function_return` of `currentFlag`, from a fresh sidecar initialised as infer's is. */
async function inferFlag(ws: Workspace): Promise<Inferred | undefined> {
  const client = new SidecarClient();
  await client.start();
  try {
    await client.send({ action: 'init', request_id: 'init', repo_root: ws.service, tsconfig_path: 'tsconfig.json' });
    const res = await client.send<{ inferred_types?: Inferred[] }>(
      {
        action: 'infer',
        request_id: 'infer',
        requests: [
          {
            file_path: ws.handler,
            line_number: SOURCE.slice(0, SOURCE.indexOf('export function currentFlag(')).split('\n').length,
            infer_kind: 'function_return',
            alias: 'CurrentFlag_Return',
          },
        ],
      },
      60_000
    );
    return (res.inferred_types ?? []).find((row) => row.alias === 'CurrentFlag_Return');
  } finally {
    await client.stop();
  }
}

function captureFlag(ws: Workspace): CaptureAliasRecord | undefined {
  const result = captureStub({
    repoRoot: ws.service,
    serviceName: 'api',
    outDir: path.join(ws.store, 'stub'),
    tsconfigPath: 'tsconfig.json',
    anchors: [
      {
        kind: 'symbol',
        alias: 'Endpoint_flag_Response',
        symbol_name: 'CurrentFlag',
        source_file: 'src/handler.ts',
        anchor_origin: 'llm-symbol',
      },
    ],
  });
  assert.ok(result.success, `capture failed: ${JSON.stringify(result.errors)}`);
  return result.aliases.find((record) => record.alias === 'Endpoint_flag_Response');
}

function spanOf(text: string): [number, number] {
  const at = SOURCE.indexOf(text);
  assert.ok(at >= 0, `the fixture must contain: ${text}`);
  return [at, at + text.length];
}

function rowAt(rows: PositionRow[], text: string, kind: string): PositionRow {
  const [start, end] = spanOf(text);
  const row = rows.find((r) => r[0] === kind && r[1] === start && r[2] === end);
  assert.ok(row, `no ${kind} row at ${start}-${end} (${text}): ${JSON.stringify(rows.filter((r) => r[1] >= start && r[2] <= end))}`);
  return row;
}

const hasError = (row: PositionRow): boolean => (row[8] & FLAG_ERROR_TYPE) !== 0;

describe('carrick#1910: the rows process reads an unbuilt workspace package from its source', () => {
  let ws: Workspace;
  let rows: PositionRow[];
  let plainRows: PositionRow[];
  let inferred: Inferred | undefined;
  let captured: CaptureAliasRecord | undefined;

  before(async () => {
    ws = materialise();
    rows = await handlerRows(ws, { workspace_source: true });
    plainRows = await handlerRows(ws, {});
    inferred = await inferFlag(ws);
    captured = captureFlag(ws);
  });

  it('a call on a receiver typed by an unbuilt workspace package names its source declaration', () => {
    const call = rowAt(rows, 'this.dep.total(account)', 'call');
    assert.ok(!hasError(call), JSON.stringify(call));
    assert.strictEqual(call[4], 'packages/ledger/src/index.ts', JSON.stringify(call));
    const free = rowAt(rows, 'makeAccount()', 'call');
    assert.ok(!hasError(free), JSON.stringify(free));
    assert.strictEqual(free[4], 'packages/ledger/src/index.ts', JSON.stringify(free));
  });

  it('an exports map into an unbuilt dist resolves to source', () => {
    const member = rowAt(rows, 'flag.enabled', 'member');
    assert.ok(!hasError(member), JSON.stringify(member));
    assert.strictEqual(member[4], 'packages/flags/src/index.ts', JSON.stringify(member));
  });

  it('two configs writing one entry from different sources stays unresolved', () => {
    assert.ok(hasError(rowAt(rows, 'piece.size', 'member')));
  });

  it('a rows process not told it is one reads the package as before', () => {
    // The known positive for the switch: without the field the loader is
    // today's, so the unbuilt package keeps the error type.
    assert.ok(hasError(rowAt(plainRows, 'this.dep.total(account)', 'call')));
    assert.ok(hasError(rowAt(plainRows, 'flag.enabled', 'member')));
  });

  it('infer, initialised without the field, still reads the unbuilt package as unresolved', () => {
    assert.ok(inferred, 'no inference');
    assert.ok(!inferred.type_string.includes('enabled: boolean'), inferred.type_string);
    assert.ok(inferred.any_provenance && inferred.any_provenance.length > 0, JSON.stringify(inferred));
  });

  it('capture still reads the unbuilt package as the top type', () => {
    // Capture builds its own programs and never the loader's: the alias
    // over the unbuilt package decays as it did before this change.
    assert.ok(captured, 'no capture record');
    assert.strictEqual(captured.top_type_at_self_check, true, JSON.stringify(captured));
    assert.strictEqual(captured.self_check, 'decayed_internal', JSON.stringify(captured));
  });
});
