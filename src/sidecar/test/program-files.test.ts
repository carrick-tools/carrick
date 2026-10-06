/**
 * carrick#2027: a process can list the files its program was built from, in
 * order, and another process given that list builds the same program.
 *
 * A process asked about a file its tsconfig does not list adds the file to its
 * program, so the program's file order follows the order the questions came
 * in. With `stableTypeOrdering` the compiler orders two types of the same
 * name by that file order, so two processes asked the same questions in
 * different orders print a union of two same-named interfaces in different
 * orders. A pool of processes answers one pass's questions in whatever order
 * each process receives them; this is what lets every process start from the
 * same program.
 *
 * The tree: two interfaces named `Dog`, in two modules outside the program
 * (`tools/` is not in the tsconfig's `include`), and a function in a third
 * that returns a union of both under the names it imports them as. The
 * process that meets the yard's module first places it first.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const FILES: Record<string, string> = {
  'tsconfig.json': JSON.stringify({
    compilerOptions: {
      target: 'ES2022',
      module: 'NodeNext',
      moduleResolution: 'NodeNext',
      strict: true,
      skipLibCheck: true,
    },
    include: ['src/**/*'],
  }),
  'src/index.ts': 'export const ready = true;\n',
  'tools/kennel/dog.ts':
    'export interface Dog {\n  bark(): string;\n}\n\nexport function kennel(d: Dog) {\n  return d.bark();\n}\n',
  'tools/yard/dog.ts':
    'export interface Dog {\n  woof(): number;\n}\n\nexport function yard(d: Dog) {\n  return d.woof();\n}\n',
  'tools/pick.ts':
    "import type { Dog as KennelDog } from './kennel/dog.js';\n" +
    "import type { Dog as YardDog } from './yard/dog.js';\n\n" +
    'export function pick(k: KennelDog, y: YardDog, flip: boolean) {\n  return flip ? k : y;\n}\n',
};

/** Each module outside the program, with the function in it a request asks about. */
const KENNEL = { file: 'tools/kennel/dog.ts', fn: 'kennel' };
const YARD = { file: 'tools/yard/dog.ts', fn: 'yard' };
const PICK = { file: 'tools/pick.ts', fn: 'pick' };

interface Answer {
  status: string;
  files?: string[];
  added?: number;
  inferred_types?: Array<{ alias: string; type_string: string }>;
  errors?: string[];
}

/**
 * How long a request may take. Each test starts processes of its own, and a
 * process's first request loads the compiler, which on a loaded machine takes
 * longer than the helper's default.
 */
const ANSWER_WITHIN_MS = 60_000;

const roots: string[] = [];
const clients: SidecarClient[] = [];

after(async () => {
  for (const client of clients) await client.stop();
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

/** The tree in a directory of its own, by its real path. */
function writeTree(files: Record<string, string>): string {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2027-')));
  roots.push(root);
  for (const [file, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), text);
  }
  return root;
}

let requests = 0;

/** A process scoped to `root`. */
async function processFor(root: string): Promise<SidecarClient> {
  const client = new SidecarClient();
  clients.push(client);
  await client.start();
  const init = await client.send<Answer>(
    { action: 'init', request_id: `init-${++requests}`, repo_root: root },
    ANSWER_WITHIN_MS
  );
  assert.strictEqual(init.status, 'ready');
  return client;
}

async function list(client: SidecarClient): Promise<string[]> {
  const answer = await client.send<Answer>(
    { action: 'list_program_files', request_id: `list-${++requests}` },
    ANSWER_WITHIN_MS
  );
  assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
  return answer.files ?? [];
}

async function add(client: SidecarClient, files: string[]): Promise<number> {
  const answer = await client.send<Answer>(
    { action: 'add_program_files', request_id: `add-${++requests}`, files },
    ANSWER_WITHIN_MS
  );
  assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
  return answer.added ?? -1;
}

/** The return type the signature pass reads for each function, asked one at a time. */
async function ask(
  client: SidecarClient,
  root: string,
  modules: Array<{ file: string; fn: string; text?: string }>
): Promise<string[]> {
  const printed: string[] = [];
  for (const { file, fn, text } of modules) {
    const lines = (text ?? FILES[file]).split('\n');
    const line = lines.findIndex((text) => text.startsWith(`export function ${fn}(`)) + 1;
    const answer = await client.send<Answer>(
      {
        action: 'infer',
        request_id: `infer-${++requests}`,
        requests: [
          { file_path: path.join(root, file), line_number: line, infer_kind: 'signature_return', alias: fn },
        ],
      },
      ANSWER_WITHIN_MS
    );
    assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
    printed.push(answer.inferred_types?.[0]?.type_string ?? '<nothing inferred>');
  }
  return printed;
}

/** Where each of `files` sits in a listed program. */
function placesOf(listed: string[], root: string, files: string[]): number[] {
  return files.map((file) => listed.indexOf(path.join(root, file)));
}

describe('list_program_files and add_program_files (carrick#2027)', () => {
  it('a process given another process\'s list holds its files in its order and prints as it does', async () => {
    const root = writeTree(FILES);

    // The yard's module first.
    const first = await processFor(root);
    const [, , firstPick] = await ask(first, root, [YARD, KENNEL, PICK]);
    const firstFiles = await list(first);
    const [yardAt, kennelAt] = placesOf(firstFiles, root, [YARD.file, KENNEL.file]);
    assert.ok(yardAt >= 0 && kennelAt > yardAt, `the yard's module is placed first: ${firstFiles.join('\n')}`);
    assert.ok(
      firstPick.includes('KennelDog') && firstPick.includes('YardDog'),
      `the union names both interfaces, or the test compares nothing: ${firstPick}`
    );

    // A process that meets the kennel's module first holds another order,
    // and prints the union the other way round.
    const unaligned = await processFor(root);
    const [, , unalignedPick] = await ask(unaligned, root, [KENNEL, YARD, PICK]);
    assert.notDeepStrictEqual(await list(unaligned), firstFiles);
    assert.notStrictEqual(unalignedPick, firstPick, 'the file order decides the union order');

    // Given the first process's list before anything else, a process holds
    // its order, and keeps it while it is asked in the other order.
    const aligned = await processFor(root);
    assert.strictEqual(await add(aligned, firstFiles), 3, 'the three modules outside the program');
    assert.deepStrictEqual(await list(aligned), firstFiles);
    const [, , alignedPick] = await ask(aligned, root, [KENNEL, YARD, PICK]);
    assert.deepStrictEqual(await list(aligned), firstFiles);
    assert.strictEqual(alignedPick, firstPick);

    // The same list again adds nothing.
    assert.strictEqual(await add(aligned, firstFiles), 0);
    assert.deepStrictEqual(await list(aligned), firstFiles);
  });

  it('leaves a file the program already holds where it is', async () => {
    const root = writeTree(FILES);
    const client = await processFor(root);
    await ask(client, root, [YARD, KENNEL]);
    const before = await list(client);

    assert.strictEqual(await add(client, [KENNEL.file, YARD.file].map((file) => path.join(root, file))), 0);
    assert.deepStrictEqual(await list(client), before);
  });

  it('adds files in the order it is given them', async () => {
    const root = writeTree(FILES);
    for (const order of [
      [YARD.file, KENNEL.file],
      [KENNEL.file, YARD.file],
    ]) {
      const client = await processFor(root);
      assert.strictEqual(await add(client, order.map((file) => path.join(root, file))), 2);
      const listed = await list(client);
      const [firstAt, secondAt] = placesOf(listed, root, order);
      assert.ok(firstAt >= 0 && secondAt > firstAt, `${order.join(' before ')}:\n${listed.join('\n')}`);
      assert.ok(
        listed.indexOf(path.join(root, 'src/index.ts')) < firstAt,
        'the tsconfig\'s files come before the added ones'
      );
    }
  });

  it('lists nothing, and builds nothing, before a request has built the program', async () => {
    const root = writeTree(FILES);
    const client = await processFor(root);
    assert.deepStrictEqual(await list(client), []);
    assert.strictEqual(await add(client, []), 0);
    assert.deepStrictEqual(await list(client), [], 'an empty add builds nothing either');

    await ask(client, root, [KENNEL]);
    const listed = await list(client);
    assert.ok(listed.includes(path.join(root, 'src/index.ts')), listed.join('\n'));
    assert.ok(listed.includes(path.join(root, KENNEL.file)), listed.join('\n'));
  });

  it('builds the program a process built, not a larger one, where a package is installed twice', async () => {
    // One package, `box@1.0.0`, installed twice: at the top and under `crate`,
    // which depends on it. The compiler folds the second copy into the first,
    // because they are the same package at the same version, so the second
    // copy's own import of `filler` (installed under `crate` only) is never
    // followed and `Box['filler']` reads the first copy's unresolved import.
    // Named as a root file, the second copy is a file of its own instead: its
    // import resolves and the type changes. A process given another's list
    // must fold the copy as the other did.
    const UNPACK = {
      file: 'tools/unpack.ts',
      fn: 'unpack',
      text: "import type { Box } from 'crate';\n\nexport function unpack(b: Box) {\n  return b.filler;\n}\n",
    };
    const box =
      "import type { Filler } from 'filler';\n\nexport interface Box {\n  filler: Filler;\n}\n";
    const pkg = (name: string) => JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' });
    const root = writeTree({
      ...FILES,
      'node_modules/box/package.json': pkg('box'),
      'node_modules/box/index.d.ts': box,
      'node_modules/crate/package.json': pkg('crate'),
      'node_modules/crate/index.d.ts': "export type { Box } from 'box';\n",
      'node_modules/crate/node_modules/box/package.json': pkg('box'),
      'node_modules/crate/node_modules/box/index.d.ts': box,
      'node_modules/crate/node_modules/filler/package.json': pkg('filler'),
      'node_modules/crate/node_modules/filler/index.d.ts': 'export interface Filler {\n  grams: number;\n}\n',
      'src/top.ts': "export type { Box } from 'box';\n",
      [UNPACK.file]: UNPACK.text,
    });

    const first = await processFor(root);
    const [firstUnpack] = await ask(first, root, [UNPACK]);
    const firstFiles = await list(first);

    const second = await processFor(root);
    assert.ok((await add(second, firstFiles)) > 0);
    assert.deepStrictEqual(await list(second), firstFiles);
    const [secondUnpack] = await ask(second, root, [UNPACK]);
    assert.strictEqual(secondUnpack, firstUnpack);

    // And the two go on alike: one more file added to each.
    for (const client of [first, second]) await add(client, [path.join(root, KENNEL.file)]);
    assert.deepStrictEqual(await list(second), await list(first));
  });

  it('builds the same program where no tsconfig names the files and the first build already held an added one', async () => {
    // With no tsconfig the project loads its source folders and builds
    // nothing until a request reads it, so the first request's file outside
    // those folders is among the first build's roots, and the compiler's
    // library files only join the roots at the next build.
    const { 'tsconfig.json': _, ...withoutTsconfig } = FILES;
    const root = writeTree(withoutTsconfig);

    const first = await processFor(root);
    await ask(first, root, [YARD]);
    await ask(first, root, [KENNEL]);
    const firstFiles = await list(first);
    assert.ok(
      firstFiles.some((file) => /\/lib\.[^/]*\.d\.ts$/.test(file)),
      `the second build's roots hold the compiler's library files: ${firstFiles.join('\n')}`
    );

    const second = await processFor(root);
    assert.ok((await add(second, firstFiles)) > 0);
    assert.deepStrictEqual(await list(second), firstFiles);

    for (const client of [first, second]) await add(client, [path.join(root, PICK.file)]);
    assert.deepStrictEqual(await list(second), await list(first));
  });

  it('reads a relative path against the init\'d root and skips a file that is not on disk', async () => {
    const root = writeTree(FILES);
    const client = await processFor(root);
    assert.strictEqual(await add(client, ['tools/gone.ts', YARD.file]), 1);
    assert.ok((await list(client)).includes(path.join(root, YARD.file)));
  });

  it('adds nothing to the default program for a file a referenced project owns', async () => {
    // A solution config: the service's tsconfig lists no files and references
    // the project that owns them (carrick#1604).
    const root = writeTree({
      'tsconfig.json': JSON.stringify({ files: [], references: [{ path: './tsconfig.src.json' }] }),
      'tsconfig.src.json': JSON.stringify({
        compilerOptions: { composite: true, strict: true, skipLibCheck: true },
        include: ['src'],
      }),
      'src/owned.ts': 'export const owned = 1;\n',
      'tools/loose.ts': 'export const loose = 2;\n',
    });
    const client = await processFor(root);

    assert.strictEqual(await add(client, [path.join(root, 'src/owned.ts')]), 0);
    assert.deepStrictEqual(await list(client), [], 'nothing built the default program');

    assert.strictEqual(await add(client, [path.join(root, 'tools/loose.ts')]), 1);
    const listed = await list(client);
    assert.ok(listed.includes(path.join(root, 'tools/loose.ts')), listed.join('\n'));
    assert.ok(!listed.includes(path.join(root, 'src/owned.ts')), listed.join('\n'));
  });

  it('needs init', async () => {
    const client = new SidecarClient();
    clients.push(client);
    await client.start();
    for (const request of [
      { action: 'list_program_files', request_id: 'no-init-list' },
      { action: 'add_program_files', request_id: 'no-init-add', files: ['/x.ts'] },
    ]) {
      const answer = await client.send<Answer>(request, ANSWER_WITHIN_MS);
      assert.strictEqual(answer.status, 'error');
      assert.match(answer.errors?.[0] ?? '', /not initialized/);
    }
  });
});
