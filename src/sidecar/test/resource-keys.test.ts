/**
 * The `resource_keys` action: the property keys at key paths of a make
 * call's type argument, with their declarations. Every input is written here:
 * an invented package `@fixture/hosted` whose `createClient<Database>()` takes
 * the type argument, and a `types.ts` whose `Database` states tables, views
 * and functions.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

interface Key {
  name: string;
  decl_file: string | null;
  decl_line: number | null;
}
interface PathAnswer {
  path: string[];
  keys: Key[];
  reason?: string;
}
interface Answer {
  paths: PathAnswer[];
  reason?: string;
}

const TYPES = [
  'export type Database = {', // 1
  '  public: {', // 2
  '    Tables: {', // 3
  '      accounts: { Row: { id: number } };', // 4
  '      notes: { Row: { id: number } };', // 5
  '      unused_table: { Row: { id: number } };', // 6
  '    };', // 7
  '    Views: {};', // 8
  '    Functions: {', // 9
  '      bump_count: { Args: { n: number }; Returns: number };', // 10
  '    };', // 11
  '  };', // 12
  '};', // 13
  '',
].join('\n');

const CLIENT = [
  "import { createClient } from '@fixture/hosted';",
  "import type { Database } from './types';",
  'export const typed = createClient<Database>("https://x", "k");',
  'export const untyped = createClient("https://x", "k");',
  '',
].join('\n');

describe('resource_keys: keys at paths of a make call type argument', () => {
  let root: string;
  let repo: string;
  let answers: Answer[];

  const span = (text: string): { file: string; start: number; end: number } => {
    const file = path.join(repo, 'src', 'client.ts');
    const source = fs.readFileSync(file, 'utf8');
    const start = source.indexOf(text);
    assert.ok(start >= 0, `${text} is in client.ts`);
    return { file, start, end: start + text.length };
  };

  before(async () => {
    root = fs.realpathSync.native(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-resource-keys-')));
    repo = path.join(root, 'checkout');
    fs.mkdirSync(path.join(repo, 'src'), { recursive: true });
    fs.mkdirSync(path.join(repo, 'node_modules', '@fixture', 'hosted'), { recursive: true });
    fs.writeFileSync(
      path.join(repo, 'node_modules', '@fixture', 'hosted', 'package.json'),
      JSON.stringify({ name: '@fixture/hosted', version: '1.0.0', types: 'index.d.ts' })
    );
    fs.writeFileSync(
      path.join(repo, 'node_modules', '@fixture', 'hosted', 'index.d.ts'),
      'export declare function createClient<Db = unknown>(url: string, key: string): { db: Db };\n'
    );
    fs.writeFileSync(path.join(repo, 'package.json'), JSON.stringify({ name: 'app', version: '1.0.0' }));
    fs.writeFileSync(
      path.join(repo, 'tsconfig.json'),
      JSON.stringify({ compilerOptions: { strict: true, module: 'commonjs', target: 'es2020', moduleResolution: 'node', lib: ['es2020'] }, include: ['src'] })
    );
    fs.writeFileSync(path.join(repo, 'src', 'types.ts'), TYPES);
    fs.writeFileSync(path.join(repo, 'src', 'client.ts'), CLIENT);
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
      const typed = span('createClient<Database>("https://x", "k")');
      const response = await client.send<{ status: string; errors?: string[]; result?: { answers: Answer[] } }>(
        {
          action: 'resource_keys',
          request_id: 'keys',
          scan_root: repo,
          queries: [
            { ...typed, type_arg: 0, paths: [['public', 'Tables'], ['public', 'Views'], ['public', 'Functions'], ['public', 'Nope']] },
            { ...span('createClient("https://x", "k")'), type_arg: 0, paths: [['public', 'Tables']] },
            { ...typed, type_arg: 1, paths: [['public', 'Tables']] },
            { ...typed, start: typed.start + 1, type_arg: 0, paths: [['public', 'Tables']] },
          ],
        },
        60_000
      );
      assert.strictEqual(response.status, 'success', JSON.stringify(response.errors));
      answers = response.result?.answers ?? [];
    } finally {
      await client.stop();
    }
  });

  after(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  it('answers once per query, in request order', () => {
    assert.strictEqual(answers.length, 4);
  });

  it('lists the three table keys with their declaration lines, and the path each was read at', () => {
    const tables = answers[0].paths[0];
    assert.deepStrictEqual(tables.path, ['public', 'Tables']);
    assert.strictEqual(tables.reason, undefined);
    assert.deepStrictEqual(tables.keys, [
      { name: 'accounts', decl_file: 'src/types.ts', decl_line: 4 },
      { name: 'notes', decl_file: 'src/types.ts', decl_line: 5 },
      { name: 'unused_table', decl_file: 'src/types.ts', decl_line: 6 },
    ]);
  });

  it('lists the function key, and no key for an empty group', () => {
    assert.deepStrictEqual(answers[0].paths[2].keys, [{ name: 'bump_count', decl_file: 'src/types.ts', decl_line: 10 }]);
    assert.deepStrictEqual(answers[0].paths[1], { path: ['public', 'Views'], keys: [] });
  });

  it('says path_missing for a path the type does not have', () => {
    assert.deepStrictEqual(answers[0].paths[3], { path: ['public', 'Nope'], keys: [], reason: 'path_missing' });
  });

  it('reads no keys from a make with no type argument, and says so', () => {
    assert.deepStrictEqual(answers[1], { paths: [], reason: 'no_type_argument' });
  });

  it('reads no keys from a type argument index the call does not have', () => {
    assert.deepStrictEqual(answers[2], { paths: [], reason: 'no_type_argument' });
  });

  it('reads no keys where no call has that span', () => {
    assert.deepStrictEqual(answers[3], { paths: [], reason: 'no_call' });
  });
});
