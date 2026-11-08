/**
 * A `require("...")` read through an `as typeof import("m")` cast
 * (carrick#3446). When the require names no file on disk, the cast names the
 * module the code means, so the call row reaches the real function. A require
 * that names a real file keeps winning, and an uncast require of a missing
 * path stays on the local binding.
 */
import { describe, it, before } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';

const here = path.dirname(fileURLToPath(import.meta.url));
// Compiled to dist/test; the fixture lives beside the sources.
const FIXTURE = path.join(here, '..', '..', 'test', 'fixtures', 'position-rows-require-cast');

type Row = [string, number, number, string | null, string | null, number | null, string | null, string | null, number];

describe('position_rows: a require read through its typeof import cast (carrick#3446)', () => {
  let files: Record<string, Row[]>;
  const sources: Record<string, string> = {};

  before(async () => {
    // A directory under the OS temp dir; the OS cleans it.
    const root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-require-cast-')));
    const repo = path.join(root, 'repo');
    fs.mkdirSync(path.join(root, 'store'));
    fs.cpSync(FIXTURE, repo, { recursive: true });
    const names = ['orders/index.ts', 'orders/local.ts'];
    for (const n of names) sources[`src/${n}`] = fs.readFileSync(path.join(repo, 'src', n), 'utf8');
    const client = new SidecarClient();
    await client.start();
    try {
      const ready = await client.send<{ status: string }>({
        action: 'init',
        request_id: 'init',
        repo_root: repo,
        tsconfig_path: 'tsconfig.json',
        scan_root: repo,
      });
      assert.strictEqual(ready.status, 'ready');
      const out = path.join(root, 'store', 'rows.jsonl.gz');
      const answer = await client.send<{ status: string; errors?: string[] }>(
        {
          action: 'position_rows',
          request_id: 'rows',
          files: names.map((n) => path.join(repo, 'src', n)),
          scan_root: repo,
          out_path: out,
          service: 'svc',
        },
        60_000
      );
      assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
      const lines = zlib
        .gunzipSync(fs.readFileSync(out))
        .toString('utf8')
        .trimEnd()
        .split('\n')
        .slice(1)
        .map((l) => JSON.parse(l) as { f: string; r: Row[] });
      files = Object.fromEntries(lines.map((l) => [l.f, l.r]));
    } finally {
      await client.stop();
    }
  });

  function rowFor(file: string, kind: string, text: string): Row {
    const source = sources[file];
    const match = (files[file] ?? []).find((r) => r[0] === kind && source.slice(r[1], r[2]) === text);
    assert.ok(match, `no ${kind} row for ${text} in ${file}`);
    return match;
  }

  const place = (row: Row): string => `${row[3]} ${row[4]}:${row[5]}`;

  it('names the real function for a destructured name when the require names no file', () => {
    assert.strictEqual(place(rowFor('src/orders/index.ts', 'call', 'formatTotal(c)')), 'formatTotal src/common/money.ts:1');
  });

  it('follows the cast, not the required path, when the two name different modules', () => {
    assert.strictEqual(place(rowFor('src/orders/local.ts', 'call', 'ft(c)')), 'formatTotal src/common/money.ts:1');
  });

  it('leaves an uncast require of a missing path on its local binding', () => {
    assert.strictEqual(place(rowFor('src/orders/index.ts', 'call', 'roundCents(c)')), 'roundCents src/orders/index.ts:2');
  });

  it('leaves a parameter typed typeof fn on the parameter', () => {
    assert.strictEqual(place(rowFor('src/orders/index.ts', 'call', 'fee(1)')), 'fee src/orders/index.ts:9');
  });

  it('keeps a member call through a cast whole-module binding on the export', () => {
    assert.strictEqual(place(rowFor('src/orders/index.ts', 'call', 'money.roundCents(c)')), 'roundCents src/common/money.ts:5');
  });
});
