/**
 * A `receiver_type` row sits at a member read's receiver
 * when the member is declared in another package than the class the receiver
 * holds (an inherited `on`): it names that class's declaration, one row per
 * named class of the receiver's type, so an alias is resolved
 * and an intersection gives two. The compiler records it; nothing is read
 * from a name. No row where the member is declared in the class's own package.
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
const FIXTURE = path.join(here, '..', '..', 'test', 'fixtures', 'position-rows-receiver-type');

type Row = [string, number, number, string | null, string | null, number | null, string | null, string | null, number];

describe('position_rows: the class a receiver of an inherited member holds', () => {
  let root: string;
  let rows: Row[];
  let source: string;
  let version: number;

  before(async () => {
    root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-receiver-type-rows-')));
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

  /** The `receiver_type` rows at `receiver`, the receiver of the read `read`. */
  const at = (read: string, receiver: string): Row[] => {
    const start = source.indexOf(read);
    assert.ok(start >= 0 && read.startsWith(receiver), read);
    return rows.filter((r) => r[0] === 'receiver_type' && r[1] === start && r[2] === start + receiver.length);
  };

  const socket = (found: Row[]): void => {
    assert.strictEqual(found.length, 1);
    const [, , , symbol, file, line, pkg, type, flags] = found[0];
    assert.strictEqual(symbol, 'Socket');
    assert.match(file ?? '', /node_modules\/wire-pkg\/index\.d\.ts$/);
    assert.strictEqual(line, 2);
    assert.strictEqual(pkg, 'wire-pkg');
    assert.strictEqual(type, null);
    assert.ok((flags & FLAG_PUBLISHED) !== 0, 'the class is published by its module');
  };

  it('names the class behind an alias', () => socket(at('aliased.on', 'aliased')));

  it('names the class of a plain receiver, read from a nullable type and a member receiver', () => {
    socket(at('plain.on', 'plain'));
    socket(at('maybe?.on', 'maybe'));
    socket(at('holder.inner.on', 'holder.inner'));
  });

  it('writes one row per named class of an intersection, in file then position order', () => {
    const found = at('both.on', 'both');
    assert.deepStrictEqual(found.map((r) => r[3]), ['Socket', 'Other']);
    assert.ok(found.every((r) => r[6] === 'wire-pkg'));
  });

  it('writes none where the member is declared in the class\'s own package', () => {
    assert.deepStrictEqual(at('plain.emit', 'plain'), []);
  });

  it('writes none for a receiver typed by an interface (widening is a deliberate change)', () => {
    assert.deepStrictEqual(at('iface.on', 'iface'), []);
  });

  it('names a class of the checkout by its own package', () => {
    const found = at('local.on', 'local');
    assert.deepStrictEqual(found.map((r) => [r[3], r[4], r[6]]), [['LocalSocket', 'src/main.ts', 'receiver-type']]);
  });

  it('names a default-exported class of an inner module by its own name, as the package publishes it', () => {
    const found = at('cursor.on', 'cursor');
    assert.deepStrictEqual(
      found.map((r) => [r[3], r[6]]),
      [['Cursor', 'wire-pkg']]
    );
    assert.match(found[0][4] ?? '', /node_modules\/wire-pkg\/cursor\.d\.ts$/);
  });

  it('names the default class of a package entry as its default', () => {
    const found = at('hub.on', 'hub');
    assert.deepStrictEqual(
      found.map((r) => [r[3], r[6]]),
      [['default', 'entry-pkg']]
    );
  });

  it('bumps the format version', () => {
    assert.strictEqual(version, POSITION_ROWS_VERSION);
    assert.strictEqual(POSITION_ROWS_VERSION, 7);
  });
});
