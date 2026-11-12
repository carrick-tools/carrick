/**
 * Locator text in the infer path's text match (carrick#3306, carrick#3307).
 *
 * The model's print of a payload has no comments and may elide the middle of
 * the node. The source has comments and every member in full. The text match
 * must read both alike, and must not read a different payload as the same.
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { SidecarClient } from './helpers.js';

const SOURCE_TS = `interface Order { id: string }
interface Reply { code(status: number): Reply; send(body?: unknown): void }
declare function toDto(order: Order): { id: string };
declare function notFound(): { statusCode: number; toJSON(): object };
declare const lines: string[];
declare const total: number;
declare const page: { map<T>(fn: (row: number) => T): T[] };

export async function withComment(order: Order | undefined, reply: Reply) {
  if (!order) { const err = notFound(); return reply.code(err.statusCode).send(err.toJSON()) }
  return reply.send({
    order: toDto(order),
    // internal id stays private
    lines,
  })
}

declare const alpha: string;
declare const beta: number;

export function shorthandComment(reply: Reply) {
  return reply.send({
    alpha,
    // internal id stays private
    beta,
  })
}

export function blockComment(order: Order, reply: Reply) {
  return reply.send({ order: toDto(order), /* kept private */ lines })
}

export function stringWithSlashes(reply: Reply) {
  return reply.send({ url: 'https://example.test/a', total })
}

export function elided(reply: Reply) {
  return reply.send({ rows: page.map(r => { const v = r; return { v } }), total })
}
`;

interface InferShape {
  inferred_types?: Array<{ alias: string; type_string: string }>;
}

const collapse = (text: string): string => text.replace(/\s+/g, ' ').trim();

function lineOf(marker: string): number {
  const index = SOURCE_TS.split('\n').findIndex((line) => line.includes(marker));
  assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
  return index + 1;
}

describe('infer: locator text ignores comments and reads a mid-text elision (#3306, #3307)', () => {
  let client: SidecarClient;
  let repoDir: string;
  let sourcePath: string;

  before(async () => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-locator-text-'));
    fs.writeFileSync(
      path.join(repoDir, 'tsconfig.json'),
      JSON.stringify({
        compilerOptions: {
          strict: true,
          module: 'esnext',
          moduleResolution: 'bundler',
          target: 'es2022',
          lib: ['es2022'],
          skipLibCheck: true,
        },
        include: ['src'],
      })
    );
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
    sourcePath = path.join(repoDir, 'src', 'source.ts');
    fs.writeFileSync(sourcePath, SOURCE_TS);

    client = new SidecarClient();
    await client.start();
    await client.send({ action: 'init', request_id: 'init', repo_root: repoDir });
  });

  after(async () => {
    await client.stop();
    fs.rmSync(repoDir, { recursive: true, force: true });
  });

  const infer = async (alias: string, expression: string, line: number): Promise<string | undefined> => {
    const response = await client.send<InferShape>({
      action: 'infer',
      request_id: `infer-${alias}`,
      requests: [
        {
          file_path: sourcePath,
          line_number: line,
          expression_text: expression,
          expression_line: line,
          infer_kind: 'response_body',
          alias,
        },
      ],
    });
    const inferred = response.inferred_types?.find((t) => t.alias === alias);
    return inferred ? collapse(inferred.type_string) : undefined;
  };

  const ELIDED_OBJECT = '{ rows: { v: number; }[]; total: number; }';
  const ORDER_LINES = '{ order: { id: string; }; lines: string[]; }';

  it('(3) locates a payload whose source holds a line comment', async () => {
    const text = await infer('Line', '{ order: toDto(order), lines }', lineOf('return reply.send({'));
    assert.strictEqual(text, ORDER_LINES);
  });

  it('(3) locates a shorthand payload with a comment line between members', async () => {
    const text = await infer('Shorthand', '{ alpha, beta }', lineOf('export function shorthandComment') + 1);
    assert.strictEqual(text, '{ alpha: string; beta: number; }');
  });

  it('(3) locates a payload whose source holds a block comment', async () => {
    const text = await infer('Block', '{ order: toDto(order), lines }', lineOf('/* kept private */'));
    assert.strictEqual(text, ORDER_LINES);
  });

  it('(4) a locator that drops the property values does not match the payload', async () => {
    const text = await infer('Negative', '{ order, lines }', lineOf('return reply.send({'));
    assert.notStrictEqual(text, ORDER_LINES);
  });

  it('(5) a // inside a string literal is not a comment', async () => {
    const text = await infer(
      'Slashes',
      "{ url: 'https://example.test/a', total }",
      lineOf('stringWithSlashes') + 1
    );
    assert.strictEqual(text, '{ url: string; total: number; }');
  });

  it('#3307 (1) locates the object for a locator that elides its middle', async () => {
    const text = await infer(
      'Elided',
      '{ rows: page.map(r => { ... }), total }',
      lineOf('export function elided') + 1
    );
    assert.strictEqual(text, ELIDED_OBJECT);
  });

  it('#3307 (2) an elision whose suffix differs does not match', async () => {
    const text = await infer(
      'OtherSuffix',
      '{ rows: page.map(r => { ... }), count }',
      lineOf('export function elided') + 1
    );
    assert.notStrictEqual(text, ELIDED_OBJECT);
  });

  it('#3307 (3) a spread is not an elision', async () => {
    const text = await infer(
      'Spread',
      '{ rows: page.map(r => { ...rest }), total }',
      lineOf('export function elided') + 1
    );
    assert.notStrictEqual(text, ELIDED_OBJECT);
  });
});
