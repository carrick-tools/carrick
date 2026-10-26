/**
 * A daemon's capture keeps what an edit cannot change, and writes the stub a
 * cold capture writes, byte for byte (carrick#2882).
 *
 * The service: an alias whose union leads with an `any` member, a contract
 * read through a barrel, a leaf, a declaration that imports a package the
 * checkout does not have (a dangling import the self-check repairs), a global
 * declaration and its reader, and a workspace package linked into
 * `node_modules` (read outside the stub). After each edit, and again after
 * it is undone, the daemon's capture and a fresh stdio process's capture of
 * the same anchors are compared: the answer, and every file of the stub, with
 * the stub directory taken out. `status` says how much each capture wrote
 * again and checked again.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';
import { SocketClient, daemon, killDaemons, recordOf, stopDaemon } from './typed-helpers.js';

const SHARED: Record<string, string> = {
  'package.json': JSON.stringify({ name: '@ws/shared', version: '1.0.0', types: 'src/index.ts' }),
  'src/index.ts': `export interface SharedDto {
  id: string;
}
`,
};

const SERVICE: Record<string, string> = {
  'tsconfig.json': JSON.stringify({
    compilerOptions: { target: 'es2020', module: 'commonjs', strict: true, skipLibCheck: true, types: [] },
    include: ['src/**/*.ts'],
  }),
  'package.json': JSON.stringify({ name: 'svc', private: true, dependencies: { '@ws/shared': '1.0.0' } }),
  // The union's members are imported in the order that numbered them backwards before stable type ordering (carrick#1977).
  'src/left.ts': `export interface Left {
  x: any;
  side: 'l';
}
`,
  'src/right.ts': `export interface Right {
  y: any;
  side: 'r';
}
`,
  'src/either.ts': `import type { Right } from './right';
import type { Left } from './left';
export type Either = Left | Right;
`,
  'src/contracts.ts': `export interface OrderDto {
  id: string;
  name: string;
}
export function makeOrder(): OrderDto {
  return { id: 'a', name: 'n' };
}
`,
  'src/index.ts': `export * from './contracts';
export * from './either';
`,
  'src/routes.ts': `import { makeOrder } from './index';
export function getOrder() {
  const order = makeOrder();
  return { ...order, seen: 1 };
}
export type GetOrder = ReturnType<typeof getOrder>;
`,
  'src/leaf.ts': `export function ping() {
  return { ok: true as boolean };
}
export type Ping = ReturnType<typeof ping>;
`,
  'src/remote.ts': `import type { Missing } from 'not-installed-pkg';
export interface UsesMissing {
  m: Missing;
  n: number;
}
`,
  'src/tagged.ts': `declare global {
  interface CarrickTag {
    tag: string;
  }
}
export {};
`,
  'src/tag-use.ts': `export function tagOf(): CarrickTag {
  return { tag: 't' } as CarrickTag;
}
export type Tagged = ReturnType<typeof tagOf>;
`,
  'src/shared-use.ts': `import type { SharedDto } from '@ws/shared';
export interface Holder {
  shared: SharedDto;
}
`,
};

const ANCHORS: Array<[string, string, string]> = [
  ['Alias_either', 'Either', 'src/either.ts'],
  ['Alias_order', 'OrderDto', 'src/contracts.ts'],
  ['Alias_get_order', 'GetOrder', 'src/routes.ts'],
  ['Alias_ping', 'Ping', 'src/leaf.ts'],
  ['Alias_missing', 'UsesMissing', 'src/remote.ts'],
  ['Alias_tagged', 'Tagged', 'src/tag-use.ts'],
  ['Alias_holder', 'Holder', 'src/shared-use.ts'],
];

const workspaces: string[] = [];

after(() => {
  killDaemons();
  for (const ws of workspaces) fs.rmSync(ws, { recursive: true, force: true });
});

function write(root: string, file: string, text: string): void {
  fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
  fs.writeFileSync(path.join(root, file), text);
}

function workspace(): { ws: string; root: string; shared: string } {
  const ws = fs.mkdtempSync(path.join(os.tmpdir(), 'tcw-'));
  workspaces.push(ws);
  const shared = path.join(ws, 'shared');
  const root = path.join(ws, 'svc');
  for (const [file, text] of Object.entries(SHARED)) write(shared, file, text);
  for (const [file, text] of Object.entries(SERVICE)) write(root, file, text);
  fs.mkdirSync(path.join(root, 'node_modules', '@ws'), { recursive: true });
  fs.symlinkSync(shared, path.join(root, 'node_modules', '@ws', 'shared'), 'dir');
  return { ws, root, shared };
}

function captureOf(root: string, outDir: string): Record<string, unknown> {
  return {
    request_id: 'c',
    action: 'capture_v2',
    repo_root: root,
    service_name: 'svc',
    out_dir: outDir,
    anchors: ANCHORS.map(([alias, symbol_name, source_file]) => ({
      kind: 'symbol',
      alias,
      symbol_name,
      source_file,
      anchor_origin: 'llm-symbol',
    })),
  };
}

/** The answer and every file of the stub, with the stub directory taken out. */
function captured(answer: Record<string, unknown>, outDir: string): string {
  assert.strictEqual(answer.status, 'success', JSON.stringify(answer));
  const files: Record<string, string> = {};
  const walk = (dir: string): void => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const abs = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(abs);
      else files[path.relative(outDir, abs)] = fs.readFileSync(abs, 'utf8');
    }
  };
  walk(outDir);
  const { request_id: _id, ...rest } = answer;
  return JSON.stringify({ answer: rest, files }).split(outDir).join('<stub>');
}

async function cold(root: string, outDir: string): Promise<string> {
  const client = new SidecarClient();
  await client.start();
  try {
    return captured(await client.send(captureOf(root, outDir), 60000), outDir);
  } finally {
    await client.stop();
  }
}

interface CaptureCounts {
  captures: number;
  emitted: number;
  emit_kept: number;
  checked: number;
  check_kept: number;
  parsed: number;
  parse_kept: number;
}

async function counts(client: SocketClient, root: string): Promise<CaptureCounts> {
  const status = await client.send({ request_id: 'st', action: 'status' });
  const captures = (status.daemon as { captures: Array<CaptureCounts & { repo_root: string }> }).captures;
  const held = captures.find((c) => c.repo_root === root);
  assert.ok(held, `no held capture for ${root} in ${JSON.stringify(captures)}`);
  return held;
}

function edit(root: string, file: string, change: (text: string) => string): () => void {
  const abs = path.join(root, file);
  const before = fs.readFileSync(abs, 'utf8');
  const next = change(before);
  assert.notStrictEqual(next, before, `the edit of ${file} changes nothing`);
  fs.writeFileSync(abs, next);
  return () => fs.writeFileSync(abs, before);
}

describe('warm capture (carrick#2882)', () => {
  it('writes again only what an edit reaches, and the stub a cold capture writes', async () => {
    const { ws, root, shared } = workspace();
    const child = await daemon(ws);
    const socket = recordOf(ws)!.socket;
    const client = await SocketClient.greeted(socket);
    let n = 0;
    const out = (side: string): string => path.join(ws, 'out', `${side}-${n}`);

    /** One capture each way; returns how much the daemon wrote again and checked again. */
    const step = async (name: string): Promise<{ emitted: number; checked: number }> => {
      n += 1;
      const before = n > 1 ? await counts(client, root) : { emitted: 0, checked: 0 };
      const warmDir = out('warm');
      const warm = captured(await client.send(captureOf(root, warmDir)), warmDir);
      const coldDir = out('cold');
      assert.strictEqual(warm, await cold(root, coldDir), `${name}: the daemon's capture differs from a cold one`);
      const now = await counts(client, root);
      return { emitted: now.emitted - before.emitted, checked: now.checked - before.checked };
    };

    const first = await step('first capture');
    assert.ok(first.emitted > 0 && first.checked > 0, JSON.stringify(first));
    const repeat = await step('repeat');
    // Only the surface entry is written again; no stub file changed.
    assert.deepStrictEqual(repeat, { emitted: 1, checked: 0 }, 'repeat');
    const leaf = await counts(client, root);
    assert.ok(leaf.parse_kept > 0, 'parsed files are kept across captures');

    // [name, edit] — each is undone, and the undone tree captured too.
    const edits: Array<[string, () => () => void]> = [
      ['leaf body', () => edit(root, 'src/leaf.ts', (t) => t.replace('return { ok: true as boolean };', 'const ok = true as boolean;\n  return { ok };'))],
      ['new export', () => edit(root, 'src/leaf.ts', (t) => `${t}export function pong() {\n  return 1;\n}\n`)],
      ['contract shape', () => edit(root, 'src/contracts.ts', (t) => t.replace('  name: string;\n', '  name: string;\n  region?: string;\n'))],
      ['contract body', () => edit(root, 'src/contracts.ts', (t) => t.replace("name: 'n'", "name: 'm'"))],
      ['the any-leading union', () => edit(root, 'src/left.ts', (t) => t.replace('  x: any;', '  x: unknown;'))],
      ['a declaration with a dangling import', () => edit(root, 'src/remote.ts', (t) => t.replace('n: number', 'n: string'))],
      // The reader's stub file is unchanged, and now names a global no file declares.
      ['a global declaration', () => edit(root, 'src/tagged.ts', (t) => t.replace('interface CarrickTag', 'interface CarrickLabel'))],
      ['the linked package', () => edit(shared, 'src/index.ts', (t) => t.replace('id: string;', 'id: string;\n  rank: number;'))],
      // The importer's stub file is unchanged, and its import no longer resolves.
      ['the linked package\'s entry', () => edit(shared, 'package.json', (t) => t.replace('src/index.ts', 'src/gone.ts'))],
      [
        'a file new to the program',
        () => {
          write(root, 'src/extra.ts', 'export const extra = { e: 1 };\n');
          const undo = edit(root, 'src/routes.ts', (t) => t.replace("return { ...order, seen: 1 };", "return { ...order, seen: 1, extra };").replace("import { makeOrder } from './index';", "import { makeOrder } from './index';\nimport { extra } from './extra';"));
          return () => {
            undo();
            fs.rmSync(path.join(root, 'src/extra.ts'));
          };
        },
      ],
    ];
    const moved: Record<string, { emitted: number; checked: number }> = {};
    for (const [name, make] of edits) {
      const undo = make();
      moved[name] = await step(name);
      undo();
      moved[`${name}, undone`] = await step(`${name}, undone`);
    }
    if (process.env.TYPED_TEST_LOG) console.error(JSON.stringify({ first, repeat, moved }));
    // A leaf body writes again the entry and the leaf, and checks nothing again.
    assert.deepStrictEqual(moved['leaf body'], { emitted: 2, checked: 0 }, JSON.stringify(moved));
    // A contract's shape reaches its reader through the barrel; the stub files of both are checked again.
    assert.ok(moved['contract shape'].emitted < first.emitted, JSON.stringify(moved));
    // A changed global declaration checks every stub file again, and so does
    // a change outside the stub that it reads.
    for (const name of ['a global declaration', 'the linked package', "the linked package's entry"]) {
      assert.ok(moved[name].checked >= first.checked, `${name}: ${JSON.stringify(moved)}`);
    }

    await stopDaemon(child, socket);
  });
});
