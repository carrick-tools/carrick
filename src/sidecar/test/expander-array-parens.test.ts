/**
 * Regression for the array-element parenthesisation in the shared structural
 * expander (#257 Copilot review). A union/intersection array element must be
 * wrapped in parens before `[]` — `(A | B)[]`, not `A | B[]` — decided from the
 * TYPE, not by string inspection (a union led by an object literal,
 * `{ a: string } | null`, must still be parenthesised; a plain object array
 * `{ a: string }[]` must NOT).
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { Project } from 'ts-morph';
import { expandTypeStructural } from '../src/type-structural-expander.js';
import { expandOriginOf } from './helpers.js';

const noWs = (s: string) => s.replace(/\s/g, '');

describe('expandTypeStructural array parenthesisation (#257)', () => {
  // strict on, so `Box | null` stays a real union (non-strict absorbs null).
  const project = new Project({
    useInMemoryFileSystem: true,
    compilerOptions: { strict: true },
  });
  const sf = project.createSourceFile(
    'arr.ts',
    `
    interface Box { a: string }
    type ArrOfUnion = (Box | null)[];
    type UnionLedByObject = ({ a: string } | null)[];
    type ArrOfObject = Box[];
    `,
  );
  const expand = (name: string) =>
    expandTypeStructural(sf.getTypeAliasOrThrow(name).getType(), expandOriginOf(project));

  it('parenthesises a union array element', () => {
    const out = expand('ArrOfUnion');
    assert.match(out, /^\(.*\)\[\]$/, `expected (…)[], got: ${out}`);
    assert.ok(out.includes('null'), `expected null member, got: ${out}`);
    assert.ok(noWs(out).includes('a:string'), `expected the Box shape, got: ${out}`);
  });

  it('parenthesises a union led by an object literal (the bug)', () => {
    const out = expand('UnionLedByObject');
    assert.match(out, /^\(.*\)\[\]$/, `expected (…)[], got: ${out}`);
    assert.ok(out.includes('null'), `expected null member, got: ${out}`);
  });

  it('does NOT parenthesise a plain object array', () => {
    const out = expand('ArrOfObject');
    assert.ok(!out.startsWith('('), `must not parenthesise an object array, got: ${out}`);
    assert.match(out, /\}\[\]$/, `expected {…}[], got: ${out}`);
  });
});
