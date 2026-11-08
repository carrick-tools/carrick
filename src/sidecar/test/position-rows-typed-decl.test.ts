/**
 * A `typed_decl` row (carrick#2448, T3a) sits at a call whose callee symbol
 * is a value (a parameter, a property, a local) and whose callee TYPE is a
 * named function's own (`typeof fetch`): it names that function's declaration.
 * The compiler records it; nothing is read from a name. No row where the
 * callee symbol already is that declaration, or where the type is anonymous.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
import { FLAG_PUBLISHED, POSITION_ROWS_VERSION } from '../src/position-rows.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(here, '..', '..', 'test', 'fixtures', 'position-rows-typed-decl');

type Row = [string, number, number, string | null, string | null, number | null, string | null, string | null, number];

describe('position_rows: a call through a value typed as a function (carrick#2448)', () => {
  let root: string;
  let rows: Row[];
  let source: string;
  let version: number;

  before(async () => {
    root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-typed-decl-rows-')));
    const repo = path.join(root, 'repo');
    fs.mkdirSync(path.join(root, 'store'));
    fs.cpSync(FIXTURE, repo, { recursive: true });
    source = fs.readFileSync(path.join(repo, 'src', 'main.ts'), 'utf8');
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
          files: [path.join(repo, 'src', 'main.ts')],
          scan_root: repo,
          out_path: out,
          service: 'svc',
        },
        60_000
      );
      assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
      const lines = zlib.gunzipSync(fs.readFileSync(out)).toString('utf8').trimEnd().split('\n');
      version = (JSON.parse(lines[0]) as { v: number }).v;
      rows = (JSON.parse(lines[1]) as { r: Row[] }).r;
    } finally {
      await client.stop();
    }
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  /** The `typed_decl` rows at the call starting `text`. */
  const typed = (text: string): Row[] => {
    const start = source.indexOf(text);
    assert.ok(start >= 0, text);
    return rows.filter((r) => r[0] === 'typed_decl' && r[1] === start);
  };

  it('names the library function a value typed `typeof fetch` is', () => {
    for (const call of ["send('/api/status')", "send('/api/param')"]) {
      const found = typed(call);
      assert.strictEqual(found.length, 1, call);
      const [, , , symbol, file, line, , type, flags] = found[0];
      assert.strictEqual(symbol, 'fetch');
      assert.match(file ?? '', /^lib:.*lib\.dom\.d\.ts$/);
      assert.ok((line ?? 0) > 0);
      assert.strictEqual(type, null);
      assert.ok((flags & FLAG_PUBLISHED) !== 0, 'a global is published');
    }
  });

  it('names a function declared in the checkout', () => {
    const found = typed("send('x')");
    assert.strictEqual(found.length, 1);
    assert.strictEqual(found[0][3], 'local');
    assert.strictEqual(found[0][4], 'src/main.ts');
  });

  it('writes no row where the callee symbol is the declaration, or the type is anonymous', () => {
    assert.strictEqual(typed("fetch('/api/direct')").length, 0);
    assert.strictEqual(typed("local('z')").length, 0);
    assert.strictEqual(typed("h('y')").length, 0);
  });

  it('bumps the format version', () => {
    assert.strictEqual(version, POSITION_ROWS_VERSION);
    assert.strictEqual(POSITION_ROWS_VERSION, 5);
  });
});
