/**
 * A package that a TypeScript file only `require`s is not in the program:
 * TypeScript follows `require("m")` only in JavaScript files, so with no
 * `import` of the package anywhere the carrick#2454 rule found no module to
 * name, and a class bound by `const { C } = require("m")` stayed at its local
 * binding (carrick#3971). The walk loads each package a TypeScript file
 * requires into the program it walks, as an `import` would.
 *
 * The fixture has nothing else that loads the package: no import, no
 * `typeof import` cast and no JavaScript file.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import * as zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';

const here = path.dirname(fileURLToPath(import.meta.url));
// Compiled to dist/test; the fixture lives beside the sources.
const FIXTURE = path.join(here, '..', '..', 'test', 'fixtures', 'position-rows-require-loads');

type Row = [string, number, number, string | null, string | null, number | null, string | null, string | null, number];

describe('position_rows: a package only required from TypeScript (carrick#3971)', () => {
  let root: string;
  let files: Record<string, Row[]>;
  const sources: Record<string, string> = {};
  const names = ['moduleRequire.ts', 'localRequire.ts', 'declaredRequire.ts', 'relative.ts', 'untyped.ts'];

  before(async () => {
    root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-require-loads-')));
    const repo = path.join(root, 'repo');
    fs.mkdirSync(path.join(root, 'store'));
    fs.cpSync(FIXTURE, repo, { recursive: true });
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

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  /** Where the `kind` row whose text starts with `text` says its symbol is declared. */
  function placeOf(file: string, kind: string, text: string): string {
    const source = sources[file];
    const row = (files[file] ?? []).find((r) => r[0] === kind && source.slice(r[1], r[2]).startsWith(text));
    assert.ok(row, `no ${kind} row for ${text} in ${file}`);
    return `${row[3]} ${row[4]}:${row[5]} ${row[6]}`;
  }

  it('names the package class at a module-level require destructure', () => {
    assert.strictEqual(placeOf('src/moduleRequire.ts', 'call', 'new Command('), 'Command node_modules/sdk/index.d.ts:2 sdk');
    assert.strictEqual(placeOf('src/moduleRequire.ts', 'call', 'new Client('), 'Client node_modules/sdk/index.d.ts:3 sdk');
  });

  it('names the package class at a function-local require destructure', () => {
    assert.strictEqual(placeOf('src/localRequire.ts', 'call', 'new Command('), 'Command node_modules/sdk/index.d.ts:2 sdk');
    assert.strictEqual(placeOf('src/localRequire.ts', 'call', 'new Client('), 'Client node_modules/sdk/index.d.ts:3 sdk');
  });

  it('reads a `declare const require` in the file as the global require', () => {
    assert.strictEqual(placeOf('src/declaredRequire.ts', 'call', 'new Command('), 'Command node_modules/sdk/index.d.ts:2 sdk');
  });

  it('loads packages only: a relative require outside the program stays at its binding', () => {
    assert.strictEqual(placeOf('src/relative.ts', 'call', 'helper()'), 'helper src/relative.ts:1 svc');
  });

  it('loads no JavaScript: a package with no types stays at its binding, as an import leaves it untyped', () => {
    assert.strictEqual(placeOf('src/untyped.ts', 'call', 'make()'), 'make src/untyped.ts:1 svc');
  });
});
