/**
 * A member read on a union of types that each declare it lists every one of
 * those declarations (carrick#2448, slice B).
 *
 * `(pick ? a : b).get()` with two classes that each declare `get`: the
 * compiler's symbol for `get` holds both declarations, and the `member` and
 * `call` rows name only the first. Each such row is followed by one `decl`
 * row per declaration. The controls write none: a union of one class with
 * `undefined`, an overloaded method, and two instantiations of one generic
 * class all reach a single declaration.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { SidecarClient, asVersion2Rows } from './helpers.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'position-rows-union-members');

type Row = [string, number, number, string | null, string | null, number | null, string | null, string | null, number];

describe('position_rows: a member on a union lists each declaration (carrick#2448)', () => {
  let root: string;
  let rows: Row[];
  let text: string;
  let source: string;

  before(async () => {
    root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-union-members-')));
    const repo = path.join(root, 'repo');
    fs.cpSync(path.join(FIXTURE, 'repo'), repo, { recursive: true });
    const main = path.join(repo, 'src', 'main.ts');
    source = fs.readFileSync(main, 'utf8');
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
          files: [main],
          scan_root: repo,
          out_path: out,
          service: 'svc',
        },
        60_000
      );
      assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
      text = zlib.gunzipSync(fs.readFileSync(out)).toString('utf8');
      rows = text
        .trimEnd()
        .split('\n')
        .slice(1)
        .flatMap((line) => (JSON.parse(line) as { r: Row[] }).r);
    } finally {
      await client.stop();
    }
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  /** The index of the one row of `kind` whose source text is exactly `span`. */
  const indexOf = (kind: string, span: string): number => {
    const found = rows.flatMap((r, i) => (r[0] === kind && source.slice(r[1], r[2]) === span ? [i] : []));
    assert.strictEqual(found.length, 1, `rows for ${kind} ${span}: ${found.length}`);
    return found[0];
  };

  /** The `decl` rows written right after the row of `kind` at `span`, as `[symbol, file, line, package, type, flags]`. */
  const declsAfter = (kind: string, span: string): unknown[][] => {
    const at = indexOf(kind, span);
    const [, start, end] = rows[at];
    const out: unknown[][] = [];
    for (let i = at + 1; i < rows.length && rows[i][0] === 'decl'; i++) {
      assert.deepStrictEqual([rows[i][1], rows[i][2]], [start, end], 'a decl row carries its row span');
      out.push(rows[i].slice(3));
    }
    return out;
  };

  it('writes format version 5', () => {
    assert.strictEqual(JSON.parse(text.split('\n')[0]).v, 5);
  });

  it('lists both classes for a member read on a union of two classes', () => {
    assert.deepStrictEqual(declsAfter('member', '(pick ? http : cached).get'), [
      ['get', 'src/transports.ts', 2, 'svc', null, 0],
      ['get', 'src/transports.ts', 8, 'svc', null, 0],
    ]);
  });

  it('lists both classes for the call of that member', () => {
    assert.deepStrictEqual(
      declsAfter('call', '(pick ? http : cached).get("/a")').map((r) => r.slice(0, 3)),
      [
        ['get', 'src/transports.ts', 2],
        ['get', 'src/transports.ts', 8],
      ]
    );
  });

  it('keeps the member row itself on the first declaration', () => {
    const row = rows[indexOf('member', '(pick ? http : cached).get')];
    assert.deepStrictEqual([row[3], row[4], row[5]], ['get', 'src/transports.ts', 2]);
  });

  it('names the mapped type for a union constituent whose member has no declaration', () => {
    assert.deepStrictEqual(
      declsAfter('member', '(pick ? mapped : http).get').map((r) => r.slice(0, 3)),
      [
        ['get', 'src/transports.ts', 2],
        ['get', 'src/transports.ts', 29],
      ]
    );
  });

  it('writes none where the union reaches one declaration', () => {
    assert.deepStrictEqual(declsAfter('member', '(options.client ?? http).get'), []);
    assert.deepStrictEqual(declsAfter('call', 'overloaded.get("/c")'), []);
    assert.deepStrictEqual(declsAfter('member', '(pick ? text : count).read'), []);
  });

  it('writes one decl row per declaration and nothing more', () => {
    assert.strictEqual(rows.filter((r) => r[0] === 'decl').length, 8);
  });

  it('leaves every row of a version-2 kind byte-identical', () => {
    // Written by version 2 over this fixture, before `decl` rows existed.
    const expected = fs.readFileSync(path.join(FIXTURE, 'v2-kinds.expected.jsonl'), 'utf8');
    assert.strictEqual(asVersion2Rows(text), expected);
  });
});
