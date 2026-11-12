/**
 * The import graph as a format the cloud reads (carrick#3101, carrick-cloud#3414).
 *
 * `reach_graph` writes `SerialGraph` version 1, and the scanner stores it in
 * each service's type-reach record. A PR scan reads it back only from its own
 * build, but the cloud reads it from every build to answer "who uses this
 * type". So the format is pinned here: `fixtures/type-users/graph.expected.json`
 * is what `reach_graph` writes for `fixtures/type-users/repo`, and carrick-cloud
 * vendors that file byte for byte, pinned by its sha256. Any change to a key,
 * a value's shape or what a value means bumps `SerialGraph.version`, and the
 * expected file is rewritten with `UPDATE_TYPE_USERS_GOLDEN=1`.
 *
 * The fixture holds each way a file can name the type `Item`, and three
 * negatives in a file that imports it: an object key and a shadowing local
 * (identifiers, so the graph keeps them) and a string (not kept). A function
 * whose return is an `Item` only by inference has no mention at all.
 */
import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SidecarClient } from './helpers.js';
import type { SerialGraph } from '../src/types.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.join(__dirname, '..', '..', 'test', 'fixtures', 'type-users');
const EXPECTED = path.join(FIXTURE, 'graph.expected.json');

/** One file of the graph with every index read back to its string. */
interface ReadFile {
  declared: string[];
  /** Each declared name, with the names its statements mention. */
  refs: Map<string, string[]>;
  exports: string[];
  /** Each import binding: the imported name (`null` for a whole module) and its origins as `file:name`. */
  bindings: Map<string, { imported: string | null; origins: string[] }>;
  whole: string[];
}

function read(graph: SerialGraph): Map<string, ReadFile> {
  const s = (at: number): string => graph.strings[at]!;
  const files = new Map<string, ReadFile>();
  for (const file of graph.files) {
    files.set(s(file.p), {
      declared: file.d.map(s),
      refs: new Map(file.r.map(([name, ...mentions]) => [s(name!), mentions.map(s)])),
      exports: file.e.map(s),
      bindings: new Map(
        file.b.map(([local, imported, origins]) => [
          s(local),
          {
            imported: imported === -1 ? null : s(imported),
            origins: Array.from({ length: origins.length / 2 }, (_, i) => `${s(origins[2 * i]!)}:${s(origins[2 * i + 1]!)}`),
          },
        ])
      ),
      whole: file.w.map(s),
    });
  }
  return files;
}

describe('SerialGraph version 1, as the cloud reads it (carrick#3101)', () => {
  let root: string;
  let graph: SerialGraph;
  let files: Map<string, ReadFile>;

  before(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), 'type-users-'));
    fs.cpSync(path.join(FIXTURE, 'repo'), root, { recursive: true });
    const client = new SidecarClient();
    await client.start();
    try {
      const ready = await client.send<{ status: string }>(
        { action: 'init', request_id: 'init', repo_root: root, tsconfig_path: 'tsconfig.json', scan_root: root },
        60_000
      );
      assert.strictEqual(ready.status, 'ready');
      const answer = await client.send<{ status: string; errors?: string[]; graph: SerialGraph }>(
        { action: 'reach_graph', request_id: 'graph' },
        60_000
      );
      assert.strictEqual(answer.status, 'success', JSON.stringify(answer.errors));
      graph = answer.graph;
    } finally {
      await client.stop();
    }
    if (process.env.UPDATE_TYPE_USERS_GOLDEN === '1') fs.writeFileSync(EXPECTED, `${JSON.stringify(graph, null, 2)}\n`);
    files = read(graph);
  });

  after(() => fs.rmSync(root, { recursive: true, force: true }));

  it('writes exactly the stored bytes the cloud vendors', () => {
    assert.strictEqual(`${JSON.stringify(graph, null, 2)}\n`, fs.readFileSync(EXPECTED, 'utf8'));
  });

  it('keeps the version-1 keys, and paths relative to the scan root', () => {
    assert.deepStrictEqual(Object.keys(graph).sort(), ['files', 'strings', 'version']);
    assert.strictEqual(graph.version, 1);
    for (const file of graph.files) {
      assert.deepStrictEqual(Object.keys(file).sort(), ['b', 'd', 'e', 'g', 'l', 'p', 'r', 's', 't', 'w']);
    }
    assert.ok(graph.strings.every((text) => !text.startsWith('/') && !text.includes(root)), graph.strings.join());
    assert.deepStrictEqual([...files.keys()], [
      'src/alias.ts',
      'src/barrel.ts',
      'src/default-item.ts',
      'src/item.ts',
      'src/listed.ts',
      'src/make.ts',
      'src/negatives.ts',
      'src/users.ts',
    ]);
  });

  // `d` is every top-level name, import bindings included: a name declared in
  // a file is in its `d` and is not a binding. Each `r` row is the declared
  // name followed by every kept identifier its statements hold, itself first.
  it('names Item from an interface field in the declaring file', () => {
    const item = files.get('src/item.ts')!;
    assert.deepStrictEqual(item.declared, ['Item', 'Holder']);
    assert.strictEqual(item.bindings.size, 0);
    assert.deepStrictEqual(item.refs.get('Holder'), ['Holder', 'Item']);
  });

  it('resolves an import type, an export list, a barrel and a default import to the declaration', () => {
    const listed = files.get('src/listed.ts')!;
    assert.deepStrictEqual(listed.declared, ['Item']);
    assert.deepStrictEqual(listed.bindings.get('Item'), { imported: 'Item', origins: ['src/item.ts:Item'] });
    assert.deepStrictEqual(listed.exports, ['Item']);
    // A barrel holds no binding: its `export *` is an export entry, and an
    // import through it resolves straight to the declaring file.
    assert.deepStrictEqual(files.get('src/barrel.ts')!.exports, ["*'./item.js'"]);
    const users = files.get('src/users.ts')!;
    assert.deepStrictEqual(users.bindings.get('BarrelItem'), { imported: 'Item', origins: ['src/item.ts:Item'] });
    assert.deepStrictEqual(users.refs.get('fromBarrel'), ['fromBarrel', 'BarrelItem']);
    // A default import resolves to the local the default export names.
    assert.deepStrictEqual(users.bindings.get('DefaultItem'), {
      imported: 'default',
      origins: ['src/default-item.ts:DefaultItem'],
    });
    const defaultItem = files.get('src/default-item.ts')!;
    assert.deepStrictEqual(defaultItem.refs.get('DefaultItem'), ['DefaultItem', 'Item']);
    assert.deepStrictEqual(defaultItem.bindings.get('Item'), { imported: 'Item', origins: ['src/item.ts:Item'] });
  });

  it('reads a namespace import as the whole module and each export, and an import() type as one binding', () => {
    const users = files.get('src/users.ts')!;
    assert.deepStrictEqual(users.bindings.get('items'), {
      imported: null,
      origins: ['src/item.ts:*', 'src/item.ts:Item', 'src/item.ts:Holder'],
    });
    assert.deepStrictEqual(users.whole, ['items']);
    assert.deepStrictEqual(users.refs.get('fromNamespace'), ['fromNamespace', 'items']);
    // `import('./item.js').Item` binds a local no identifier can spell (it
    // starts with a NUL), imports no name, and resolves to the declaration.
    const alias = files.get('src/alias.ts')!;
    const [local, ...rest] = [...alias.bindings.keys()];
    assert.deepStrictEqual(rest, []);
    assert.ok(local!.startsWith('\u0000'), JSON.stringify(local));
    assert.deepStrictEqual(alias.bindings.get(local!), { imported: null, origins: ['src/item.ts:Item'] });
    assert.deepStrictEqual(alias.refs.get('ItemAlias'), ['ItemAlias', local]);
  });

  it('keeps an object key and a shadowing local, and no string or inferred return', () => {
    const negatives = files.get('src/negatives.ts')!;
    assert.deepStrictEqual(negatives.refs.get('written'), ['written', 'Item']);
    assert.deepStrictEqual(negatives.refs.get('keyOnly'), ['keyOnly', 'Item']);
    assert.deepStrictEqual(negatives.refs.get('shadowed'), ['shadowed', 'Item']);
    assert.deepStrictEqual(negatives.refs.get('stringOnly'), ['stringOnly']);
    assert.deepStrictEqual(negatives.refs.get('inferredOnly'), ['inferredOnly', 'makeItem']);
  });
});
