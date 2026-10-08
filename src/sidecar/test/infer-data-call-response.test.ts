/**
 * Regression for carrick#2055 (cause A): a route whose located response is a
 * call that PRODUCES data was published with what went into the call.
 *
 * The response read keeps a transitional drill for a located send
 * (`res.json(users)` -> `users`). Two gates decided when it ran, and both read
 * a data call as a send:
 *
 *  - a call handed an inline function was taken for a route registration, so
 *    `rows.map((r) => ({ ... }))` published the callback's return: the element,
 *    not the list;
 *  - a call whose element type is declared in a package was taken for a
 *    library describing itself, so `rows.map(toDto)` published the mapper's
 *    function type and `listItems(ownerId)` published `string`.
 *
 * A call whose result is plain data (an object with no callable member, or an
 * array of one, once promise levels and `null`/`undefined` are taken off) is
 * the payload, whatever its arguments and wherever its type is declared.
 * Everything else keeps its path: a send whose result is a library builder
 * with methods, a send whose result is `void`, a registration whose result
 * carries the handler, and a call whose result is a primitive (a serialiser).
 *
 * No library, framework or method name is matched.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

/** A contracts package the service reads its wire types from, and a reply
 * library whose send hands back a builder. */
const CONTRACTS_DTS = `export interface ItemDto {
  id: string;
  title: string;
}
export interface ItemPage {
  items: ItemDto[];
  total: number;
}
`;

const REPLY_DTS = `export interface Reply {
  code(status: number): Reply;
  sent: boolean;
}
export declare function respond(body: unknown): Reply;
`;

const SERVICE_TS = `import type { ItemDto, ItemPage } from "@acme/contracts";
import { respond } from "reply-kit";

type Row = { id: string; title: string; ownerId: string; secret: string };

declare const rows: Row[];
declare function send(body: unknown): void;
declare function loadRows(ownerId: string): Promise<Row[]>;
declare function encode(body: unknown): string;
declare function route(path: string, handler: () => unknown): { path: string; handler: () => unknown };

function toDto(row: Row): ItemDto {
  return { id: row.id, title: row.title };
}

function listItems(ownerId: string): ItemDto[] {
  return rows.filter((r) => r.ownerId === ownerId).map(toDto);
}

async function pageOf(ownerId: string): Promise<ItemPage> {
  const loaded = await loadRows(ownerId);
  return { items: loaded.map(toDto), total: loaded.length };
}

export function inlineMapped() {
  return send(rows.map((r) => ({ id: r.id, title: r.title })));
}

export function namedMapped() {
  return send(rows.map(toDto));
}

export function listed(ownerId: string) {
  return send(listItems(ownerId));
}

export async function paged(ownerId: string) {
  return send(await pageOf(ownerId));
}

export function filtered(ownerId: string) {
  return send(rows.filter((r) => r.ownerId === ownerId));
}

export function builder(ownerId: string) {
  return respond(listItems(ownerId));
}

export function serialised(ownerId: string) {
  return send(encode(listItems(ownerId)));
}

export const registered = route("/items", () => rows.map(toDto));
`;

const lineOf = (text: string): number => {
  const at = SERVICE_TS.indexOf(text);
  assert.ok(at >= 0, `fixture must contain: ${text}`);
  assert.strictEqual(SERVICE_TS.indexOf(text, at + 1), -1, `fixture must contain exactly one: ${text}`);
  return SERVICE_TS.slice(0, at).split('\n').length;
};

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

describe('carrick#2055: a located call that produces data is the payload, not a send', () => {
  let client: SidecarClient;
  let repoDir: string;
  let servicePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-2055-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: { strict: true, module: 'esnext', moduleResolution: 'bundler', target: 'es2022', lib: ['es2022'] },
        include: ['src'],
      })
    );
    const packages: Array<[string, string]> = [
      ['@acme/contracts', CONTRACTS_DTS],
      ['reply-kit', REPLY_DTS],
    ];
    for (const [name, dts] of packages) {
      const dir = path.join(repoDir, 'node_modules', ...name.split('/'));
      fs.mkdirSync(dir, { recursive: true });
      fs.writeFileSync(path.join(dir, 'package.json'), JSON.stringify({ name, version: '1.0.0', types: 'index.d.ts' }));
      fs.writeFileSync(path.join(dir, 'index.d.ts'), dts);
    }
    servicePath = path.join(repoDir, 'src', 'service.ts');
    fs.writeFileSync(servicePath, SERVICE_TS);
    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init-2055', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (alias: string, expression: string, at: string): Promise<string | undefined> => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: servicePath,
          line_number: lineOf(at),
          expression_text: expression,
          expression_line: lineOf(at),
          infer_kind: 'response_body',
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    return inferred ? collapse(inferred.type_string) : undefined;
  };

  // The package element prints by its name, as it does when the same list
  // is read out of a send; the bundle carries its declaration.
  const LIST = 'ItemDto[]';

  it('publishes the list an inline-callback map builds, not the callback return', async () => {
    const text = await infer(
      'InlineMapped',
      'rows.map((r) => ({ id: r.id, title: r.title }))',
      'return send(rows.map((r) => ({ id: r.id, title: r.title })));'
    );
    assert.strictEqual(text, '{ id: string; title: string; }[]');
  });

  it('publishes the list a named mapper builds, not the mapper', async () => {
    const text = await infer('NamedMapped', 'rows.map(toDto)', 'return send(rows.map(toDto));');
    assert.strictEqual(text, LIST);
  });

  it('publishes what a repo function returns, not its argument', async () => {
    const text = await infer('Listed', 'listItems(ownerId)', 'return send(listItems(ownerId));');
    assert.strictEqual(text, LIST);
  });

  it('publishes what an awaited repo function resolves to, not its argument or the promise', async () => {
    const text = await infer('Paged', 'pageOf(ownerId)', 'return send(await pageOf(ownerId));');
    assert.strictEqual(text, 'ItemPage');
  });

  it('publishes the rows a filter keeps, not the predicate return', async () => {
    const text = await infer(
      'Filtered',
      'rows.filter((r) => r.ownerId === ownerId)',
      'return send(rows.filter((r) => r.ownerId === ownerId));'
    );
    assert.ok(text !== undefined, 'expected a type');
    assert.ok(text.endsWith('[]'), `the filtered list is an array, got: ${text}`);
    assert.ok(text.includes('secret: string'), `the filter keeps the row, got: ${text}`);
    assert.notStrictEqual(text, 'boolean');
  });

  // Controls: the drill and the registration path still serve what they are for.

  it('still reads the payload inside a send that returns a library builder', async () => {
    const text = await infer('Builder', 'respond(listItems(ownerId))', 'return respond(listItems(ownerId));');
    assert.strictEqual(text, LIST);
  });

  it('still reads the payload inside a send the types say returns nothing', async () => {
    const text = await infer('Sent', 'send(rows.map(toDto))', 'return send(rows.map(toDto));');
    assert.strictEqual(text, LIST);
  });

  it('still reads through a serialiser whose result is a string', async () => {
    const text = await infer('Serialised', 'encode(listItems(ownerId))', 'return send(encode(listItems(ownerId)));');
    assert.strictEqual(text, LIST);
  });

  it('still follows a registration whose result carries the handler', async () => {
    const text = await infer(
      'Registered',
      'route("/items", () => rows.map(toDto))',
      'export const registered = route("/items", () => rows.map(toDto));'
    );
    assert.strictEqual(text, LIST);
  });
});
