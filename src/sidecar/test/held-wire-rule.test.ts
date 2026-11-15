import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import {
  absentBodyRule,
  jsonWireDeclarations,
  jsonWireRule,
  judgeTolerances,
  unwidenedAliasOf,
  voidReceiverDeclaration,
} from '../src/capture/check-probe.js';
import { nativeTsc, scratch, writeShadow } from './held-shadow-support.js';
import { SIDECAR_PATH } from './helpers.js';

const here = path.dirname(fileURLToPath(import.meta.url));
const FIXTURE = path.resolve(here, '..', '..', 'test', 'fixtures', 'held-node');

/** The native compiler's error lines for one standalone file (strict, no libs beyond es2022). */
function tscErrors(source: string): string[] {
  const dir = scratch('held-wire-rule-');
  const file = path.join(dir, 'a.ts');
  fs.writeFileSync(file, source);
  const run = spawnSync(nativeTsc(), ['--noEmit', '--strict', '--target', 'es2022', '--lib', 'es2022', '--ignoreConfig', '--pretty', 'false', file], {
    cwd: dir,
    encoding: 'utf8',
  });
  return run.stdout.split('\n').filter((l) => /error TS\d+/.test(l));
}

function actions(repo: string, lines: object[]): Array<Record<string, unknown>> {
  const run = spawnSync('node', [SIDECAR_PATH], {
    cwd: os.tmpdir(),
    input: [{ action: 'init', request_id: 'i', repo_root: repo, scan_root: repo }, ...lines, { action: 'shutdown', request_id: 'x' }]
      .map((l) => JSON.stringify(l))
      .join('\n') + '\n',
    encoding: 'utf8',
    timeout: 120_000,
    maxBuffer: 64 * 1024 * 1024,
  });
  return run.stdout
    .split('\n')
    .filter((l) => l.trim().startsWith('{'))
    .map((l) => JSON.parse(l) as Record<string, unknown>);
}

const WIRE_HEAD = (wire: string[], expected: string): string =>
  [
    ...wire,
    `declare function send1<V>(v: V, c: (o: __carrick_Wire<V, ${expected}>) => ${expected}): V;`,
    'send1({ at: new Date() }, (o) => o);',
  ].join('\n') + '\n';

describe('the wire rule from one writer', () => {
  it('(b) write_shadow carries the declarations and the judge\'s own rule', () => {
    const shadow = writeShadow(FIXTURE, scratch('held-wire-b-'));
    assert.deepEqual(shadow.wire, [
      ...jsonWireDeclarations('__carrick_'),
      `type __carrick_Wire<S, E> = ${jsonWireRule('S', 'E', '__carrick_')};`,
    ]);
    assert.equal('hono' in shadow, false);
  });

  it('(b2) the rule text is the judge\'s, prefixed and parameterised', () => {
    assert.equal(
      jsonWireRule('S', 'E', ''),
      '[S] extends [E] ? S : (JsonWireSame<JsonWire<S>, S> extends true ? S : JsonWire<S>)'
    );
  });

  it('(c) a Date sent against a string passes; against a number it fails', () => {
    const shadow = writeShadow(FIXTURE, scratch('held-wire-c-'));
    assert.deepEqual(tscErrors(WIRE_HEAD(shadow.wire, '{ at: string }')), []);
    assert.ok(tscErrors(WIRE_HEAD(shadow.wire, '{ at: number }')).length > 0);
  });

  it('(d) the judge\'s three tolerances travel with the wire rule, read from its own code', () => {
    const shadow = writeShadow(FIXTURE, scratch('held-wire-d-'));
    assert.deepEqual(shadow.tolerances, judgeTolerances());
    assert.equal(judgeTolerances().unwidened_alias_suffix, unwidenedAliasOf(''));
    assert.equal(judgeTolerances().void_receiver, voidReceiverDeclaration());
    assert.equal(judgeTolerances().absent_body.declared, absentBodyRule('S', 'E', false));
    assert.equal(judgeTolerances().absent_body.by_handler, absentBodyRule('S', 'E', true));
  });

  it('(d2) absent body: dropped when the receiver admits absence, or its handler tolerates it', () => {
    const body = (expected: string, tolerant: boolean): string =>
      `type S = { a: number } | undefined;\ntype E = ${expected};\ntype Out = ${absentBodyRule('S', 'E', tolerant)};\nconst out: E = null as unknown as Out;\n`;
    assert.deepEqual(tscErrors(body('{ a?: number } | undefined', false)), []);
    assert.ok(tscErrors(body('{ a?: number }', false)).length > 0, 'a receiver that does not admit absence keeps it');
    assert.deepEqual(tscErrors(body('{ a?: number }', true)), []);
    assert.ok(tscErrors(body('{ a: number }', true)).length > 0, 'a required member is no tolerance');
  });

  it('(d3) void receiver: void is read, any and a shape are not', () => {
    const src = (t: string): string => `${voidReceiverDeclaration()}\nconst x: IsVoid<${t}> = true;\n`;
    assert.deepEqual(tscErrors(src('void')), []);
    assert.ok(tscErrors(src('any')).length > 0);
    assert.ok(tscErrors(src('{ a: 1 }')).length > 0);
  });
});

describe('find_site (the scan\'s own site finder, through the engine)', () => {
  const repo = scratch('held-wire-site-');
  fs.mkdirSync(path.join(repo, 'src'));
  fs.writeFileSync(path.join(repo, 'tsconfig.json'), JSON.stringify({ compilerOptions: { strict: true, noEmit: true }, include: ['src'] }));
  const text = 'export const a = 1;\nexport function f() {\n  return { ok:   true,\n    n: 2 };\n}\n';
  fs.writeFileSync(path.join(repo, 'src', 'a.ts'), text);

  it('(e) finds the exact-text node: offsets, lines, kind', () => {
    const answers = actions(repo, [{ action: 'find_site', request_id: 'f', file: 'src/a.ts', text: '{ ok: true, n: 2 }' }]);
    const a = answers.find((x) => x.request_id === 'f') as Record<string, unknown>;
    assert.equal(a.status, 'success', JSON.stringify(a));
    const start = text.indexOf('{ ok');
    assert.deepEqual([a.found, a.start, a.end, a.start_line, a.end_line, a.kind], [true, start, text.indexOf('};', start) + 1, 3, 4, 'ObjectLiteralExpression']);
  });

  it('(f) reads the text the caller sends instead of the disk, and says not found for no exact match', () => {
    const edited = text.replace('a = 1', 'a = 11');
    const hit = actions(repo, [{ action: 'find_site', request_id: 'f', file: 'src/a.ts', text: '11', file_text: edited }]).find((x) => x.request_id === 'f') as Record<string, unknown>;
    assert.equal(hit.found, true);
    assert.equal(hit.start, edited.indexOf('11'));
    const miss = actions(repo, [{ action: 'find_site', request_id: 'f', file: 'src/a.ts', text: 'no such expression' }]).find((x) => x.request_id === 'f') as Record<string, unknown>;
    assert.equal(miss.status, 'success');
    assert.equal(miss.found, false);
  });

  it('(g) from_line skips a match that ends above it', () => {
    const two = 'const k = foo(1);\nconst m = foo(1);\n';
    const a = actions(repo, [{ action: 'find_site', request_id: 'f', file: 'src/b.ts', text: 'foo(1)', from_line: 2, file_text: two }]).find((x) => x.request_id === 'f') as Record<string, unknown>;
    assert.equal(a.start_line, 2);
  });

  it('(h) refuses a file outside the scanned repo', () => {
    const a = actions(repo, [{ action: 'find_site', request_id: 'f', file: '../outside.ts', text: 'x', file_text: 'x' }]).find((x) => x.request_id === 'f') as Record<string, unknown>;
    assert.equal(a.status, 'error');
  });
});
