/**
 * carrick#2113: a type the source states belongs to the value it is stated
 * on. The stated-type reading behind `call_result` and `request_body` took
 * the nearest annotated declaration above the located node, or the nearest
 * cast anywhere above it, without asking where the node sat in it. So:
 *
 * - an object literal passed to a call whose result is bound to an annotated
 *   name, or cast, was published as that result's type (the response DTO
 *   stood in for the request body);
 * - a call inside a callback inside an annotated table or service object was
 *   published as the table's type.
 *
 * The reading now climbs only through what leaves a value as it is
 * (parentheses, `await`, `!`, `satisfies`, a `.catch` / `.finally` on it):
 * the first cast met, or the declaration the value initialises, answers.
 * Anything else (a call argument, an array or object member, a nested
 * function) ends the climb with nothing, and the checker's own type stands.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SOURCE_TS = `interface ItemDto { id: string; title: string; createdAt: string }
interface ItemUpsert { title: string; tags?: string[] }
interface CommandSpec { name: string; run: () => Promise<void> }
declare const client: {
  putItem(id: string, body: ItemUpsert): Promise<ItemDto>;
  list(): Promise<ItemDto[]>;
};
declare function print(x: unknown): void;

export async function rename(id: string, title: string) {
  const result: ItemDto = await client.putItem(id, { title, tags: [] }); // @bound-literal
  print(result.id);
}

export async function castResult(id: string, apiKey: string) {
  const reply = (await client.putItem(id, { title: apiKey })) as { id: string }; // @cast-literal
  print(reply.id);
}

export async function renameTyped(id: string, title: string) {
  const body: ItemUpsert = { title };
  await client.putItem(id, body); // @typed-binding
  await client.putItem(id, { title } as ItemUpsert); // @cast-argument
}

export const COMMANDS: readonly CommandSpec[] = [
  { name: 'list', run: async () => { print(await client.list()); } }, // @table
];

export async function read() {
  const items: ItemDto[] = await client.list(); // @bound-result
  const one = (await client.list()) as ItemDto[]; // @cast-result
  const pair: ItemDto[] = [await client.putItem('a', { title: 'a' })]; // @array-element
  return [items, one, pair];
}

export const api: { load: (id: string) => Promise<ItemDto> } = {
  load: (id) => client.putItem(id, { title: id }), // @service-object
};

export async function tolerant(res: Response) {
  return (await res.json().catch(() => ({}))) as Partial<ItemDto>; // @catch-link
}
`;

const ITEM_DTO = '{ id: string; title: string; createdAt: string; }';
const ITEM_DTO_LIST = /^(ItemDto|\{ id: string; title: string; createdAt: string; \})\[\]$/;

interface Inferred {
  alias: string;
  type_string: string;
  is_explicit: boolean;
}

interface InferShape {
  inferred_types?: Inferred[];
  errors?: string[];
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#2113: a stated type answers only the value it is stated on', () => {
  let client: SidecarClient;
  let repoDir: string;
  let sourcePath: string;

  before(async () => {
    repoDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2113-')));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022', 'dom'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    sourcePath = path.join(repoDir, 'src', 'items.ts');
    fs.writeFileSync(sourcePath, SOURCE_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  /** Infer `text` on the line marked `marker`, located by its byte span. */
  async function infer(
    marker: string,
    text: string,
    kind: 'call_result' | 'request_body'
  ): Promise<Inferred> {
    const lines = SOURCE_TS.split('\n');
    const index = lines.findIndex((line) => line.includes(`// @${marker}`));
    assert.ok(index >= 0, `fixture must mark ${marker}`);
    const lineStart = lines.slice(0, index).reduce((sum, line) => sum + line.length + 1, 0);
    const column = lines[index].indexOf(text);
    assert.ok(column >= 0, `line ${marker} must contain ${text}`);
    assert.strictEqual(lines[index].indexOf(text, column + 1), -1, `${text} once on ${marker}`);
    const alias = `${marker}:${kind}`;
    const res = await client.send<InferShape>({
      action: 'infer',
      request_id: alias,
      requests: [
        {
          file_path: sourcePath,
          line_number: index + 1,
          span_start: lineStart + column,
          span_end: lineStart + column + text.length,
          infer_kind: kind,
          alias,
          expression_text: text,
          expression_line: index + 1,
        },
      ],
    });
    const inferred = (res.inferred_types ?? []).find((t) => t.alias === alias);
    assert.ok(inferred, `${alias} must be answered: ${JSON.stringify(res.errors)}`);
    return { ...inferred, type_string: collapse(inferred.type_string) };
  }

  describe('a body passed to a call', () => {
    it("is not the type of the call's annotated result", async () => {
      const inferred = await infer('bound-literal', '{ title, tags: [] }', 'request_body');
      assert.notStrictEqual(inferred.type_string, ITEM_DTO);
      assert.match(inferred.type_string, /^\{ title: string; tags: never\[\]; \}$/);
    });

    it("is not the type of a cast on the call's result", async () => {
      const inferred = await infer('cast-literal', '{ title: apiKey }', 'request_body');
      assert.strictEqual(inferred.type_string, '{ title: string; }');
    });

    it('keeps a typed binding passed by name', async () => {
      const inferred = await infer('typed-binding', 'body', 'request_body');
      assert.match(inferred.type_string, /^(ItemUpsert|\{ title: string; tags\?: string\[\]( \| undefined)?; \})$/);
    });

    it('keeps a cast written on the argument itself', async () => {
      const inferred = await infer('cast-argument', '{ title } as ItemUpsert', 'request_body');
      assert.match(inferred.type_string, /^\{ title: string; tags\?: string\[\]( \| undefined)?; \}$/);
      assert.strictEqual(inferred.is_explicit, true);
    });

    it('is not the type of the service object its callback sits in', async () => {
      const inferred = await infer('service-object', '{ title: id }', 'request_body');
      assert.strictEqual(inferred.type_string, '{ title: string; }');
    });
  });

  describe("a call's result", () => {
    it('is not the type of the table its callback sits in', async () => {
      const inferred = await infer('table', 'client.list()', 'call_result');
      assert.doesNotMatch(inferred.type_string, /CommandSpec|name: string; run/);
      assert.match(inferred.type_string, /^(ItemDto\[\]|\{ id: string; title: string; createdAt: string; \}\[\])$/);
    });

    it('is not the type of the array it is an element of', async () => {
      const inferred = await infer('array-element', "client.putItem('a', { title: 'a' })", 'call_result');
      assert.match(inferred.type_string, /^(ItemDto|\{ id: string; title: string; createdAt: string; \})$/);
    });

    it('is not the type of the service object its callback sits in', async () => {
      const inferred = await infer('service-object', 'client.putItem(id, { title: id })', 'call_result');
      assert.match(inferred.type_string, /^(ItemDto|\{ id: string; title: string; createdAt: string; \})$/);
      assert.doesNotMatch(inferred.type_string, /load/);
    });

    it('keeps the annotation of the name it initialises', async () => {
      const inferred = await infer('bound-result', 'client.list()', 'call_result');
      assert.match(inferred.type_string, ITEM_DTO_LIST);
    });

    it('keeps a cast on it', async () => {
      const inferred = await infer('cast-result', 'client.list()', 'call_result');
      assert.match(inferred.type_string, ITEM_DTO_LIST);
    });

    it('keeps a cast written after a `.catch` on it', async () => {
      const inferred = await infer('catch-link', 'res.json()', 'call_result');
      assert.match(inferred.type_string, /^\{ id\?: string( \| undefined)?; title\?: string( \| undefined)?; createdAt\?: string( \| undefined)?; \}$/);
      assert.strictEqual(inferred.is_explicit, true);
    });
  });
});
