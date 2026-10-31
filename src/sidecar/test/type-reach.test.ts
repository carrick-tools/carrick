/**
 * Which files an edit reaches (carrick#3100).
 *
 * Each case builds a small tree, reads its import graph from a real program,
 * and asserts exactly the files `reach` answers. Every answer is also taken
 * from the live graph (`affectedFiles`) and must equal the answer over the
 * serialised graph: dropping statement texts and unrelated identifiers loses
 * nothing the walk reads.
 */

import { describe, it, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { Project } from 'ts-morph';
import {
  affectedFiles,
  importGraphOf,
  reach,
  serialiseGraph,
  type FileChange,
  type ReachResult,
} from '../src/affected-set.js';
import { SidecarClient } from './helpers.js';

const BASE: Record<string, string> = {
  'tsconfig.json': JSON.stringify({
    compilerOptions: {
      target: 'es2020',
      module: 'esnext',
      moduleResolution: 'bundler',
      strict: true,
      skipLibCheck: true,
      allowJs: true,
      noEmit: true,
      types: [],
    },
    include: ['src'],
  }),
  'package.json': JSON.stringify({ name: 'svc', private: true }),
  // A contracts folder re-exported by a barrel.
  'src/contracts/orders.ts': 'export interface OrderDto {\n  id: string;\n}\nexport const unusedFlag = 1;\n',
  'src/contracts/users.ts': 'export interface UserDto {\n  name: string;\n}\n',
  'src/contracts/index.ts': "export * from './orders';\nexport * from './users';\n",
  'src/uses-orders.ts': "import { OrderDto } from './contracts';\nexport const order: OrderDto = { id: 'a' };\n",
  'src/uses-users.ts': "import { UserDto } from './contracts/users';\nexport type User = UserDto;\n",
  // A name declared from another in the same file.
  'src/schema.ts': 'export const schema = { a: 1 };\nexport type Dto = typeof schema;\nexport const other = 2;\n',
  'src/dto-user.ts': "import { Dto } from './schema';\nexport type Wrapped = Dto[];\n",
  'src/chain.ts': "import { Wrapped } from './dto-user';\nexport type Chained = Wrapped;\n",
  'src/other-user.ts': "import { other } from './schema';\nexport const twice = other * 2;\n",
  // A name imported from a file that is not the one under edit.
  'src/elsewhere.ts': 'export const extra = 1;\nexport interface Settings {\n  mode: string;\n}\n',
  'src/imports-extra.ts': "import { extra } from './elsewhere';\nexport const more = extra + 1;\n",
  // An augmentation of a module of the repo.
  'src/augment.ts':
    "import { UserDto } from './contracts/users';\ndeclare module './elsewhere' {\n  interface Settings {\n    owner?: UserDto;\n  }\n}\nexport {};\n",
  // A script: no import or export.
  'src/globals.d.ts': 'interface Env {\n  region: string;\n}\n',
  // A folder module and the files that read it.
  'src/y/index.ts': 'export const y = 1;\n',
  'src/imports-y.ts': "import { y } from './y';\nexport const v = y;\n",
  'src/via-barrel.ts': "export * from './y';\n",
  'src/uses-barrel.ts': "import { y } from './via-barrel';\nexport const w = y;\n",
  // An import that resolves to nothing yet.
  'src/missing-user.ts': "import { gone } from './gone';\nexport const g = gone;\n",
  // CommonJS.
  'src/cjs/lib.js': 'module.exports = { a: 1 };\n',
  'src/cjs/uses.js': "const lib = require('./lib');\nmodule.exports = { v: lib.a };\n",
  'src/cjs/other.js': 'module.exports = {};\n',
  'src/cjs/unrelated.js': "const k = require('./other');\nmodule.exports = { k };\n",
};

const roots: string[] = [];

after(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

function tree(files: Record<string, string> = BASE): string {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'reach-'));
  roots.push(root);
  for (const [file, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), text);
  }
  return root;
}

/** The change that rewrites `file` of the base tree with `edit`. */
function edit(file: string, edited: (text: string) => string): FileChange {
  const before = BASE[file]!;
  return { file, before, after: edited(before) };
}

const add = (file: string, text: string): FileChange => ({ file, before: '', after: text });
const remove = (file: string): FileChange => ({ file, before: BASE[file]!, after: '' });

let cached: { root: string; graph: ReturnType<typeof importGraphOf> } | undefined;

/** The graph of the base tree, read once. */
function baseGraph() {
  if (!cached) {
    const root = tree();
    const project = new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') });
    cached = { root, graph: importGraphOf(project.getProgram().compilerObject, root) };
  }
  return cached.graph;
}

/** What `reach` answers over the serialised graph, which must equal the live walk. */
function reachOf(...changes: FileChange[]): ReachResult {
  const graph = baseGraph();
  const live = affectedFiles(graph, changes);
  const serial = reach(JSON.parse(JSON.stringify(serialiseGraph(graph))), changes);
  if (live === 'all') {
    assert.strictEqual(serial.reached, 'all');
  } else {
    assert.deepStrictEqual(serial, {
      reached: [...live.files].sort(),
      augmented: [...live.augmented].sort(),
      exports_moved: live.exportsMoved,
    });
  }
  return serial;
}

describe('reach (carrick#3100)', () => {
  it('reaches the importers of an edited export through a barrel, and not a file that reads another export', () => {
    const answer = reachOf(edit('src/contracts/orders.ts', (t) => t.replace('id: string;', 'id: number;')));
    assert.deepStrictEqual(answer.reached, ['src/contracts/orders.ts', 'src/uses-orders.ts']);
    assert.deepStrictEqual(answer.augmented, []);
    assert.strictEqual(answer.exports_moved, false);
  });

  it('reaches only the changed file when nothing reads the edited name', () => {
    const answer = reachOf(edit('src/contracts/orders.ts', (t) => t.replace('unusedFlag = 1', 'unusedFlag = 2')));
    assert.deepStrictEqual(answer.reached, ['src/contracts/orders.ts']);
  });

  it('follows a name declared from another in the same file, and on through the importers', () => {
    const answer = reachOf(edit('src/schema.ts', (t) => t.replace('{ a: 1 }', '{ a: 1, b: 2 }')));
    assert.deepStrictEqual(answer.reached, ['src/chain.ts', 'src/dto-user.ts', 'src/schema.ts']);
  });

  it('reaches only the added file when a file is added', () => {
    const answer = reachOf(add('src/fresh.ts', 'export const fresh = 1;\nexport type Fresh = string;\n'));
    assert.deepStrictEqual(answer.reached, ['src/fresh.ts']);
    assert.deepStrictEqual(answer.augmented, []);
    assert.strictEqual(answer.exports_moved, false);
  });

  it('reaches the importers of a deleted file, and the barrel that looked it up', () => {
    const answer = reachOf(remove('src/contracts/users.ts'));
    assert.deepStrictEqual(answer.reached, ['src/augment.ts', 'src/contracts/index.ts', 'src/uses-users.ts']);
    assert.deepStrictEqual(answer.augmented, ['src/augment.ts']);
  });

  it('does not match the export names of an added file against importers of that name elsewhere', () => {
    const answer = reachOf(add('src/more-extras.ts', 'export const extra = 2;\n'));
    assert.deepStrictEqual(answer.reached, ['src/more-extras.ts']);
  });

  it('keeps the held rule for a modified file that adds an export', () => {
    const answer = reachOf(edit('src/contracts/orders.ts', (t) => `${t}export const extra = 3;\n`));
    assert.deepStrictEqual(answer.reached, ['src/contracts/orders.ts', 'src/imports-extra.ts']);
    assert.strictEqual(answer.exports_moved, true);
  });

  it('returns an edited augmentation in augmented, and does not answer all', () => {
    const answer = reachOf(edit('src/augment.ts', (t) => t.replace('owner?: UserDto', 'owner: UserDto')));
    assert.deepStrictEqual(answer.augmented, ['src/augment.ts']);
    assert.deepStrictEqual(answer.reached, ['src/augment.ts']);
  });

  it('returns an augmentation that reads an edited name in augmented', () => {
    const answer = reachOf(edit('src/contracts/users.ts', (t) => t.replace('name: string', 'name: number')));
    assert.deepStrictEqual(answer.augmented, ['src/augment.ts']);
    assert.deepStrictEqual(answer.reached, ['src/augment.ts', 'src/contracts/users.ts', 'src/uses-users.ts']);
  });

  it('returns an added declaration script and an edited one in augmented', () => {
    const added = reachOf(add('src/env.d.ts', 'interface Region {\n  code: string;\n}\n'));
    assert.deepStrictEqual(added.augmented, ['src/env.d.ts']);
    assert.deepStrictEqual(added.reached, ['src/env.d.ts']);
    const edited = reachOf(edit('src/globals.d.ts', (t) => t.replace('string', 'number')));
    assert.deepStrictEqual(edited.augmented, ['src/globals.d.ts']);
    assert.notStrictEqual(edited.reached, 'all');
  });

  it('reaches only the require importers of an edited CommonJS module', () => {
    const answer = reachOf(edit('src/cjs/lib.js', (t) => t.replace('a: 1', 'a: 2')));
    assert.deepStrictEqual(answer.reached, ['src/cjs/lib.js', 'src/cjs/uses.js']);
    assert.deepStrictEqual(answer.augmented, []);
  });

  it('reaches the importers of ./y when a y.ts is added beside y/index.ts', () => {
    const answer = reachOf(add('src/y.ts', "export const y = 'shadow';\n"));
    assert.deepStrictEqual(answer.reached, [
      'src/imports-y.ts',
      'src/uses-barrel.ts',
      'src/via-barrel.ts',
      'src/y.ts',
    ]);
  });

  it('reaches the file whose unresolved import an added file fills', () => {
    const answer = reachOf(add('src/gone.ts', 'export const gone = 1;\n'));
    assert.deepStrictEqual(answer.reached, ['src/gone.ts', 'src/missing-user.ts']);
  });

  it('reaches nothing else when an added file shadows nothing', () => {
    const answer = reachOf(add('src/z.ts', 'export const z = 1;\n'));
    assert.deepStrictEqual(answer.reached, ['src/z.ts']);
  });

  it('answers all for an installed file and a json file', () => {
    assert.strictEqual(reachOf({ file: 'node_modules/p/index.d.ts', before: 'a', after: 'b' }).reached, 'all');
    assert.strictEqual(reachOf({ file: 'src/data.json', before: '{}', after: '{"a":1}' }).reached, 'all');
  });

  it('reads a file the program takes as a script as a script when its text reads as a module', () => {
    // `module.exports` in a `.cjs` file with no hint of its own: the text says
    // module, the program says script. A script is returned in augmented.
    const files = { ...BASE, 'src/legacy.js': 'var shared = 1;\n' };
    const root = tree(files);
    const project = new Project({ tsConfigFilePath: path.join(root, 'tsconfig.json') });
    const graph = importGraphOf(project.getProgram().compilerObject, root);
    const answer = affectedFiles(graph, [
      { file: 'src/legacy.js', before: files['src/legacy.js']!, after: 'var shared = 2;\nexports.x = shared;\n' },
    ]);
    assert.notStrictEqual(answer, 'all');
    assert.deepStrictEqual([...(answer as { augmented: Set<string> }).augmented], ['src/legacy.js']);
  });
});

describe('reach_graph and reach actions (carrick#3100)', () => {
  it('serialises repo files only, relative to the scan root, and answers reach without init', async () => {
    const root = tree({
      ...BASE,
      'node_modules/pkg/package.json': JSON.stringify({ name: 'pkg', version: '1.0.0', types: 'index.d.ts' }),
      'node_modules/pkg/index.d.ts': 'export declare const p: number;\n',
      'src/uses-pkg.ts': "import { p } from 'pkg';\nexport const q = p;\n",
    });
    const client = new SidecarClient();
    await client.start();
    try {
      // Before init the graph cannot be read, but a reach over a graph can.
      const early = await client.send<{ status: string }>({ action: 'reach_graph', request_id: 'early' });
      assert.strictEqual(early.status, 'error');

      const init = await client.send<{ status: string }>(
        { action: 'init', request_id: 'init', repo_root: root, scan_root: root },
        60000
      );
      assert.strictEqual(init.status, 'ready');
      const answer = await client.send<{
        status: string;
        version: number;
        graph: Parameters<typeof reach>[0];
        errors?: string[];
      }>({ action: 'reach_graph', request_id: 'graph' }, 60000);
      assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
      assert.strictEqual(answer.version, 1);
      const paths = answer.graph.files.map((file) => answer.graph.strings[file.p]!);
      assert.ok(paths.includes('src/uses-orders.ts'));
      assert.ok(paths.every((file) => !file.startsWith('/') && !file.includes('node_modules')), paths.join());
      assert.ok(answer.graph.strings.every((text) => !text.includes('export interface')), 'no statement text');

      const change = edit('src/contracts/orders.ts', (t) => t.replace('id: string;', 'id: number;'));
      const asked = await client.send<ReachResult & { status: string }>({
        action: 'reach',
        request_id: 'reach',
        graph: answer.graph,
        changes: [change],
      });
      assert.strictEqual(asked.status, 'success');
      assert.deepStrictEqual(asked.reached, ['src/contracts/orders.ts', 'src/uses-orders.ts']);
      assert.deepStrictEqual(asked.augmented, []);
      assert.strictEqual(asked.exports_moved, false);
    } finally {
      await client.stop();
    }
  });

  it('refuses a graph with an index outside its string table', async () => {
    const client = new SidecarClient();
    await client.start();
    try {
      const asked = await client.send<{ status: string; errors?: string[] }>({
        action: 'reach',
        request_id: 'bad',
        graph: {
          version: 1,
          strings: ['a.ts'],
          files: [{ p: 9, g: 0, d: [], r: [], l: [], e: [], t: [], b: [], w: [], s: [] }],
        },
        changes: [],
      });
      assert.strictEqual(asked.status, 'error');
    } finally {
      await client.stop();
    }
  });
});
