/**
 * A held service answers what a cold process would after files change
 * (carrick#2880).
 *
 * One daemon holds a service while the test edits it on disk the way an
 * editor, a hook or a branch switch would. After each edit, `infer` and
 * `list_program_files` through the daemon are compared byte for byte with a
 * fresh stdio process given the same requests (only the timing fields
 * `init_time_ms` and `infer_timing` taken out), and `status` says which path
 * the daemon took:
 * - refresh (changed files read again): a body edit, a new export, a type's
 *   shape changed through a barrel;
 * - none: a file touched without a change;
 * - drop (built again from its `init`): an import added, a file added to and
 *   removed from the config's file list, a tsconfig option, a package.json,
 *   and more changed files than a refresh takes.
 *
 * The re-answer when files change while a request runs is pinned on
 * `answerWhileStable` and `HeldFiles` directly, with a real project.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import { SidecarClient } from './helpers.js';
import { SocketClient, daemon, killDaemons, recordOf, stopDaemon, untimed } from './typed-helpers.js';
import { HeldFiles, MAX_REFRESHED_FILES, answerWhileStable } from '../src/held-freshness.js';

const GENERATED = MAX_REFRESHED_FILES + 1;

const TSCONFIG = {
  compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] as string[] },
  include: ['src/**/*.ts'],
};

const FILES: Record<string, string> = {
  'src/contract.ts': `export interface Item {
  id: string;
  qty: number;
}
`,
  'src/util.ts': `export function count(n: number) {
  return n * 2;
}
`,
  'src/index.ts': `export * from './contract';
export * from './util';
`,
  'src/handler.ts': `import { Item, count } from './index';
export function list(): Item[] {
  return [{ id: 'a', qty: 1 }];
}
export function view(item: Item) {
  return { ...item, label: count(item.qty) };
}
`,
};

const workspaces: string[] = [];

after(() => {
  killDaemons();
  for (const ws of workspaces) fs.rmSync(ws, { recursive: true, force: true });
});

function service(): { ws: string; root: string } {
  const ws = fs.mkdtempSync(path.join(os.tmpdir(), 'ctyped-'));
  workspaces.push(ws);
  const root = path.join(ws, 'svc');
  write(root, 'tsconfig.json', JSON.stringify(TSCONFIG));
  write(root, 'package.json', JSON.stringify({ name: 'svc', private: true }));
  for (const [file, text] of Object.entries(FILES)) write(root, file, text);
  for (let i = 0; i < GENERATED; i += 1) write(root, `src/gen/g${i}.ts`, `export const g${i} = ${i};\n`);
  return { ws, root };
}

function write(root: string, file: string, text: string): void {
  fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
  fs.writeFileSync(path.join(root, file), text);
}

function edit(root: string, file: string, change: (text: string) => string): void {
  const abs = path.join(root, file);
  const before = fs.readFileSync(abs, 'utf8');
  const after = change(before);
  assert.notStrictEqual(after, before, `the edit of ${file} changes nothing`);
  fs.writeFileSync(abs, after);
}

function requests(root: string): Record<string, unknown>[] {
  const handler = path.join(root, 'src', 'handler.ts');
  return [
    { request_id: 'q', action: 'infer', requests: [
      { file_path: handler, line_number: 2, infer_kind: 'function_return' },
      { file_path: handler, line_number: 5, infer_kind: 'function_return' },
    ] },
    { request_id: 'l', action: 'list_program_files' },
  ];
}

async function cold(root: string): Promise<string[]> {
  const client = new SidecarClient();
  await client.start();
  try {
    const init = await client.send<Record<string, unknown>>({ request_id: 'i', action: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready', JSON.stringify(init));
    const out: string[] = [];
    for (const request of requests(root)) out.push(untimed(await client.send(request, 60000)));
    return out;
  } finally {
    await client.stop();
  }
}

async function warm(client: SocketClient, root: string): Promise<string[]> {
  const out: string[] = [];
  for (const request of requests(root)) out.push(untimed(await client.send(request)));
  return out;
}

async function freshnessOf(client: SocketClient): Promise<{ refreshes: number; drops: number }> {
  const status = await client.send({ request_id: 'st', action: 'status' });
  return (status.daemon as { services: { freshness: { refreshes: number; drops: number } }[] }).services[0]!.freshness;
}

describe('held service after edits (carrick#2880)', () => {
  it('answers as a cold process after each kind of edit, refreshing or rebuilding as the edit needs', async () => {
    const { ws, root } = service();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    const client = await SocketClient.greeted(socket);
    const init = await client.send({ request_id: 'i', action: 'init', repo_root: root });
    assert.strictEqual(init.status, 'ready', JSON.stringify(init));

    const steps: [string, () => void, 'none' | 'refresh' | 'drop'][] = [
      ['first answer', () => {}, 'none'],
      ['body edit', () => edit(root, 'src/handler.ts', (t) => t.replace('label: count(item.qty)', 'label: count(item.qty), extra: true')), 'refresh'],
      ['new export', () => edit(root, 'src/util.ts', (t) => `${t}export const flag = true;\n`), 'refresh'],
      ['shape through a barrel', () => edit(root, 'src/contract.ts', (t) => t.replace('qty: number;', 'qty: number;\n  note?: string;')), 'refresh'],
      ['touch only', () => {
        const file = path.join(root, 'src', 'util.ts');
        const later = new Date(Date.now() + 5000);
        fs.utimesSync(file, later, later);
      }, 'none'],
      ['import added', () => edit(root, 'src/handler.ts', (t) => `import { flag } from './util';\n${t.replace('extra: true', 'extra: flag')}`), 'drop'],
      ['file added', () => write(root, 'src/extra.ts', `declare global { interface Extra { on: true } }\nexport {};\n`), 'drop'],
      ['file removed', () => fs.rmSync(path.join(root, 'src', 'extra.ts')), 'drop'],
      ['tsconfig option', () => write(root, 'tsconfig.json', JSON.stringify({ ...TSCONFIG, compilerOptions: { ...TSCONFIG.compilerOptions, exactOptionalPropertyTypes: true } })), 'drop'],
      ['package.json', () => write(root, 'package.json', JSON.stringify({ name: 'svc', private: true, type: 'module' })), 'drop'],
      ['many files', () => {
        for (let i = 0; i < GENERATED; i += 1) write(root, `src/gen/g${i}.ts`, `export const g${i} = ${i + 1};\n`);
      }, 'drop'],
    ];
    let previous = { refreshes: 0, drops: 0 };
    for (const [name, change, path_] of steps) {
      change();
      const got = await warm(client, root);
      const want = await cold(root);
      assert.deepStrictEqual(got, want, `${name}: warm and cold answers differ`);
      assert.ok(got[0]!.includes('"status":"success"'), `${name}: ${got[0]}`);
      const freshness = await freshnessOf(client);
      assert.deepStrictEqual(
        [freshness.refreshes - previous.refreshes, freshness.drops - previous.drops],
        [path_ === 'refresh' ? 1 : 0, path_ === 'drop' ? 1 : 0],
        `${name}: [refreshes, drops] taken, expected ${path_}`
      );
      previous = freshness;
    }
    client.conn.destroy();
    await stopDaemon(child, socket);
  });
});

describe('a change while a request runs (carrick#2880)', () => {
  function heldProject(): { root: string; file: string; project: Project; held: HeldFiles } {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cheld-'));
    workspaces.push(root);
    write(root, 'tsconfig.json', JSON.stringify(TSCONFIG));
    write(root, 'src/a.ts', 'export const a = 1;\n');
    const project = new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') });
    const held = new HeldFiles({ projects: () => [project], refreshable: () => true, roots: () => [root] });
    return { root, file: path.join(root, 'src', 'a.ts'), project, held };
  }

  it('drops an answer read while a file changed and answers again from the refreshed program', () => {
    const { root, project, held } = heldProject();
    let runs = 0;
    const freshen = () => {
      const found = held.sweep();
      if (found.outcome === 'refresh') held.refresh(found.files);
    };
    const answer = answerWhileStable(
      () => {
        runs += 1;
        const text = project.getSourceFileOrThrow(path.join(root, 'src', 'a.ts')).getFullText();
        if (runs === 1) write(root, 'src/a.ts', 'export const a = 2;\n');
        return text;
      },
      freshen,
      () => held.sweep().outcome === 'none',
      3
    );
    assert.strictEqual(runs, 2);
    assert.strictEqual(answer, 'export const a = 2;\n');
  });

  it('gives up after its attempts when files keep changing', () => {
    const { root, project, held } = heldProject();
    let runs = 0;
    const answer = answerWhileStable(
      () => {
        runs += 1;
        write(root, 'src/a.ts', `export const a = ${runs + 10};\n`);
        return project.getSourceFiles().length;
      },
      () => {
        const found = held.sweep();
        if (found.outcome === 'refresh') held.refresh(found.files);
      },
      () => held.sweep().outcome === 'none',
      3
    );
    assert.strictEqual(runs, 3);
    assert.strictEqual(answer, null);
  });

  it('finds a removed program file as a drop, never a refresh', () => {
    const { file, held } = heldProject();
    assert.strictEqual(held.sweep().outcome, 'none');
    fs.rmSync(file);
    const found = held.sweep();
    assert.strictEqual(found.outcome, 'drop');
  });
});
