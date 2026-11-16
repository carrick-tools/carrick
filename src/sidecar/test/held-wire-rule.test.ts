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
import { judgeFitOptions } from '../src/capture/index.js';
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
    assert.deepEqual(shadow.tolerances, judgeTolerances('__carrick_'));
    const t = judgeTolerances('__carrick_');
    assert.equal(t.unwidened_alias_suffix, unwidenedAliasOf(''));
    assert.deepEqual(t.declarations, [
      voidReceiverDeclaration('__carrick_'),
      `type __carrick_AbsentBody<S, E> = ${absentBodyRule('S', 'E', false)};`,
      `type __carrick_AbsentBodyByHandler<S, E> = ${absentBodyRule('S', 'E', true)};`,
    ]);
  });

  it('(d2) absent body: dropped when the receiver admits absence, or its handler tolerates it', () => {
    const decls = judgeTolerances('__carrick_').declarations.join('\n');
    const body = (expected: string, alias: string): string =>
      `${decls}\ntype S = { a: number } | undefined;\ntype E = ${expected};\nconst out: E = null as unknown as ${alias}<S, E>;\n`;
    assert.deepEqual(tscErrors(body('{ a?: number } | undefined', '__carrick_AbsentBody')), []);
    assert.ok(tscErrors(body('{ a?: number }', '__carrick_AbsentBody')).length > 0, 'a receiver that does not admit absence keeps it');
    assert.deepEqual(tscErrors(body('{ a?: number }', '__carrick_AbsentBodyByHandler')), []);
    assert.ok(tscErrors(body('{ a: number }', '__carrick_AbsentBodyByHandler')).length > 0, 'a required member is no tolerance');
  });

  it('(d3) void receiver: void is read, any and a shape are not', () => {
    const decls = judgeTolerances('__carrick_').declarations.join('\n');
    const src = (t: string): string => `${decls}\nconst x: __carrick_IsVoid<${t}> = true;\n`;
    assert.deepEqual(tscErrors(src('void')), []);
    assert.ok(tscErrors(src('any')).length > 0);
    assert.ok(tscErrors(src('{ a: 1 }')).length > 0);
  });
});

describe('the options a held check is armed under', () => {
  const optionsOf = (compilerOptions: Record<string, unknown>): { service: unknown; judge: unknown } => {
    const repo = scratch('held-fit-options-');
    fs.mkdirSync(path.join(repo, 'src'));
    fs.writeFileSync(path.join(repo, 'src', 'a.ts'), 'export const a = 1;\n');
    fs.writeFileSync(path.join(repo, 'tsconfig.json'), JSON.stringify({ compilerOptions: { noEmit: true, ...compilerOptions }, include: ['src'] }));
    return (writeShadow(repo, scratch('held-fit-out-')) as unknown as { options: { service: unknown; judge: unknown } }).options;
  };
  const fit = (snc: boolean, sft: boolean, exact: boolean): object => ({ strictNullChecks: snc, strictFunctionTypes: sft, exactOptionalPropertyTypes: exact });

  it('(i) the judge\'s set is read from its own tsconfig', () => {
    assert.deepEqual(optionsOf({ strict: true }).judge, fit(true, true, false));
    assert.deepEqual(judgeFitOptions(), fit(true, true, false));
  });

  it('(j) strict implies the two strict options when they are unset; an explicit value wins', () => {
    assert.deepEqual(optionsOf({ strict: true }).service, fit(true, true, false));
    assert.deepEqual(optionsOf({}).service, fit(false, false, false));
    assert.deepEqual(optionsOf({ strict: true, strictNullChecks: false }).service, fit(false, true, false));
    assert.deepEqual(optionsOf({ strict: true, strictFunctionTypes: false }).service, fit(true, false, false));
    assert.deepEqual(optionsOf({ strict: true, exactOptionalPropertyTypes: true }).service, fit(true, true, true));
  });

  it('(k) an option that changes the type an expression has is not one of them', () => {
    assert.deepEqual(optionsOf({ strict: true, noUncheckedIndexedAccess: true }).service, fit(true, true, false));
  });
});

describe('find_site (the scan\'s own site finder, through the engine)', () => {
  const repo = scratch('held-wire-site-');
  fs.mkdirSync(path.join(repo, 'src'));
  fs.writeFileSync(path.join(repo, 'tsconfig.json'), JSON.stringify({ compilerOptions: { strict: true, noEmit: true }, include: ['src'] }));
  const ask = (q: Record<string, unknown>): Record<string, unknown> =>
    actions(repo, [{ action: 'find_site', request_id: 'f', file: 'src/a.ts', ...q }]).find((x) => x.request_id === 'f') as Record<string, unknown>;
  const oldText = 'export const a = 1;\nexport function f() {\n  return { ok:   true,\n    n: 2 };\n}\n';
  const span = (text: string, of: string): { start: number; end: number } => ({ start: text.indexOf(of), end: text.indexOf(of) + of.length });

  it('(e) finds the node the old span named, in the new text', () => {
    const newText = '// a line\n' + oldText.replace('ok:   true', 'ok: true');
    const a = ask({ oldText, newText, ...span(oldText, '{ ok:   true,\n    n: 2 }') });
    assert.equal(a.status, 'success', JSON.stringify(a));
    const start = newText.indexOf('{ ok');
    assert.deepEqual([a.start, a.end, a.reason], [start, newText.indexOf('};', start) + 1, undefined]);
  });

  it('(f) says value_not_a_node when no node has the old span\'s text, or only a property does', () => {
    assert.equal(ask({ oldText, newText: 'export const q = 2;\n', ...span(oldText, '{ ok:   true,\n    n: 2 }') }).reason, 'value_not_a_node');
    const prop = 'const o = { k: 1 };\n';
    assert.equal(ask({ oldText: prop, newText: prop, ...span(prop, 'k: 1') }).reason, 'value_not_a_node');
  });

  it('(g) the match at or below the old span\'s line wins over one above it', () => {
    const t = 'const k = foo(1);\nconst m = foo(1);\n';
    const a = ask({ oldText: t, newText: t, ...span(t.slice(18), 'foo(1)'), start: 18 + t.slice(18).indexOf('foo(1)'), end: 18 + t.slice(18).indexOf('foo(1)') + 6 });
    assert.equal(a.start, t.lastIndexOf('foo(1)'));
  });

  it('(h) says not_wrappable for a spread and a shorthand property\'s name', () => {
    const sp = 'const a = [...xs];\n';
    assert.equal(ask({ oldText: sp, newText: sp, ...span(sp, '...xs') }).reason, 'not_wrappable');
    const sh = 'const o = { xs };\n';
    assert.equal(ask({ oldText: sh, newText: sh, ...span(sh, 'xs') }).reason, 'not_wrappable');
  });
});
