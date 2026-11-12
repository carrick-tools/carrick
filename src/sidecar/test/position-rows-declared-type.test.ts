/**
 * A `receiver_type` row with FLAG_DECLARED_TYPE says which package alias a
 * receiver was declared as ("declared, not held"): a make's declared return
 * type, or an annotation walked through in-repo aliases. The value's own type
 * cannot say it (the compiler prints an alias of an alias, or an intersection
 * alias, by what it expands to). At most one such row per receiver span, for a
 * type alias a package declares; the class rows are unchanged.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
import { FLAG_DECLARED_TYPE, POSITION_ROWS_VERSION } from '../src/position-rows.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(here, '..', '..', 'test', 'fixtures', 'position-rows-declared-type');
const RUST_ROWS = path.join(here, '..', '..', '..', 'position_rows.rs');

type Row = [string, number, number, string | null, string | null, number | null, string | null, string | null, number];

describe('position_rows: the package alias a receiver is declared as', () => {
  let root: string;
  let rows: Row[];
  let source: string;
  let version: number;

  before(async () => {
    root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-declared-type-rows-')));
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

  const declared = (r: Row): boolean => r[0] === 'receiver_type' && (r[8] & FLAG_DECLARED_TYPE) !== 0;

  /** The declared-type rows at `receiver`, the receiver of the read `read` (`nth` occurrence). */
  const at = (read: string, receiver: string, nth = 0): Row[] => {
    let start = -1;
    for (let i = 0; i <= nth; i++) start = source.indexOf(read, start + 1);
    assert.ok(start >= 0 && read.startsWith(receiver), read);
    return rows.filter((r) => declared(r) && r[1] === start && r[2] === start + receiver.length);
  };

  const named = (found: Row[], name: string, pkg: string): void => {
    assert.strictEqual(found.length, 1);
    assert.strictEqual(found[0][3], name);
    assert.strictEqual(found[0][6], pkg);
    assert.match(found[0][4] ?? '', new RegExp(`node_modules/${pkg}/index\\.d\\.ts$`));
    assert.strictEqual(found[0][7], null);
  };

  it('names the package alias a field is annotated as, through two in-repo aliases', () => {
    named(at('this.client.a', 'this.client'), 'ProxyOf', 'proxy-lib');
    named(at('this.other.b', 'this.other'), 'ProxyOf', 'proxy-lib');
  });

  it('names the make\'s declared return for a const with no annotation', () => {
    named(at('made.a', 'made'), 'ProxyOf', 'proxy-lib');
    named(at('handle.call', 'handle'), 'HandleOf', 'proxy-lib');
  });

  it('names an intersection alias a make declares', () => {
    named(at('hooks.x', 'hooks'), 'HooksOf', 'hooks-lib');
  });

  it('names the first package alias of a union annotation', () => {
    named(at('p?.a', 'p'), 'ProxyOf', 'proxy-lib');
  });

  it('writes none for an interface, a local type or a key read on the proxy', () => {
    assert.deepStrictEqual(at('plain.query', 'plain'), []);
    assert.deepStrictEqual(at('local.query', 'local'), []);
    assert.deepStrictEqual(at('this.client.a.query', 'this.client.a'), []);
  });

  it('writes at most one declared row per receiver span', () => {
    const seen = new Set<string>();
    for (const r of rows.filter(declared)) {
      const key = `${r[1]}:${r[2]}`;
      assert.ok(!seen.has(key), `two declared rows at ${key}`);
      seen.add(key);
    }
  });

  it('bumps the format and pins the flag to the Rust constant', () => {
    assert.strictEqual(version, POSITION_ROWS_VERSION);
    assert.strictEqual(POSITION_ROWS_VERSION, 7);
    assert.strictEqual(FLAG_DECLARED_TYPE, 256);
    const rust = fs.readFileSync(RUST_ROWS, 'utf8');
    const match = /pub const FLAG_DECLARED_TYPE: u16 = (\d+);/.exec(rust);
    assert.ok(match, 'src/position_rows.rs declares FLAG_DECLARED_TYPE');
    assert.strictEqual(Number(match[1]), FLAG_DECLARED_TYPE);
  });
});
