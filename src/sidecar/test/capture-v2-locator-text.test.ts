/**
 * Locator text in the capture's text match (carrick#3306, carrick#3307).
 *
 * The capture finds a node by comparing normalised source text with the
 * locator's normalised text. When the match fails, the anchor falls to the
 * first expression on the line (a `reply.send(` call, typed `void`). Two
 * things made it fail: a comment inside the payload (the model's print has
 * none), and a locator that elides the middle of the node (`{ ... }`).
 */

import { describe, it, before, after } from 'node:test';
import * as assert from 'node:assert';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { captureStub } from '../src/capture/index.js';
import type { CaptureStubResult } from '../src/capture/api.js';

const SOURCE = `interface Order { id: string }
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

export async function withoutComment(order: Order | undefined, reply: Reply) {
  if (!order) { const err = notFound(); return reply.code(err.statusCode).send(err.toJSON()) }
  return reply.send({
    order: toDto(order),
    lines,
  })
}

export function stringWithSlashes(reply: Reply) {
  return reply.send({ url: 'https://example.test/a', total })
}

export function elided(reply: Reply) {
  return reply.send({ rows: page.map(r => { const v = r; return { v } }), total })
}
`;

const lineOf = (marker: string): number => {
  const index = SOURCE.split('\n').findIndex((line) => line.includes(marker));
  assert.ok(index >= 0, `fixture has no line holding ${JSON.stringify(marker)}`);
  return index + 1;
};

describe('capture v2: locator text normalisation and mid-text elision (#3306, #3307)', () => {
  let repoDir: string;
  let outRoot: string;
  let result: CaptureStubResult;
  let surface: string;

  const anchor = (alias: string, expression_text: string, line_number: number) => ({
    kind: 'infer' as const,
    alias,
    source_file: 'src/routes.ts',
    anchor_origin: 'deterministic-infer' as const,
    expression_text,
    line_number,
  });

  before(() => {
    repoDir = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-locator-text-'));
    outRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'carrick-locator-text-out-'));
    fs.mkdirSync(path.join(repoDir, 'src'), { recursive: true });
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
    fs.writeFileSync(path.join(repoDir, 'src', 'routes.ts'), SOURCE);
    result = captureStub({
      repoRoot: repoDir,
      serviceName: 'locator-text-svc',
      outDir: path.join(outRoot, 'stub'),
      anchors: [
        anchor('Endpoint_comment_Response', '{ order: toDto(order), lines }', lineOf('return reply.send({')),
        anchor(
          'Endpoint_plain_Response',
          '{ order: toDto(order), lines }',
          lineOf('return reply.send({') + 8
        ),
        anchor('Endpoint_slashes_Response', "{ url: 'https://example.test/a', total }", lineOf('stringWithSlashes') + 1),
        anchor(
          'Endpoint_elided_Response',
          '{ rows: page.map(r => { ... }), total }',
          lineOf('export function elided') + 1
        ),
        anchor(
          'Endpoint_elided_other_suffix_Response',
          '{ rows: page.map(r => { ... }), count }',
          lineOf('export function elided') + 1
        ),
        anchor(
          'Endpoint_negative_Response',
          '{ order, lines }',
          lineOf('return reply.send({')
        ),
      ],
    });
    assert.strictEqual(result.success, true, JSON.stringify(result.errors));
    surface = fs.readFileSync(path.join(result.stub_dir, 'types', 'surface.d.ts'), 'utf8');
  });

  after(() => {
    fs.rmSync(repoDir, { recursive: true, force: true });
    fs.rmSync(outRoot, { recursive: true, force: true });
  });

  const aliasText = (alias: string): string => {
    const start = surface.indexOf(`${alias} =`);
    assert.ok(start >= 0, `no ${alias} in:\n${surface}`);
    const next = surface.indexOf('\nexport ', start);
    return surface.slice(start, next < 0 ? undefined : next).replace(/\s+/g, ' ');
  };

  it('(1) locates a payload whose source holds a comment line', () => {
    assert.match(aliasText('Endpoint_comment_Response'), /order: \{ id: string; \}; lines: string\[\]; \}/);
  });

  it('(2) locates the same payload with no comment, a trailing comma in the source', () => {
    assert.match(aliasText('Endpoint_plain_Response'), /order: \{ id: string; \}; lines: string\[\]; \}/);
  });

  it('(4) a locator that drops the property values does not match the payload', () => {
    assert.doesNotMatch(aliasText('Endpoint_negative_Response'), /order: \{ id: string; \}/);
  });

  it('(5) a // inside a string literal is not a comment', () => {
    assert.match(aliasText('Endpoint_slashes_Response'), /url: string; total: number; \}/);
  });

  it('#3307 (1) locates the object for a locator that elides its middle', () => {
    assert.match(aliasText('Endpoint_elided_Response'), /rows: \{ v: number; \}\[\]; total: number; \}/);
  });

  it('#3307 (2) an elision whose suffix differs does not match', () => {
    assert.doesNotMatch(aliasText('Endpoint_elided_other_suffix_Response'), /rows:/);
  });
});
